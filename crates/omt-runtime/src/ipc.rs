//! Local IPC transport (KTD2): JSON-RPC 2.0 over a unix domain socket
//! (POSIX) or a named pipe (Windows). Framing is NEWLINE-DELIMITED JSON —
//! one request/response/notification object per line, pinned in README.md.

use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
pub struct Listener {
    inner: std::os::unix::net::UnixListener,
    #[allow(dead_code)] // surfaced through local_path()
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Listener {
    /// Bind the endpoint. A leftover socket file from a dead predecessor is
    /// removed first — the caller only binds after WINNING the bootstrap
    /// election, so any existing file is stale by definition.
    pub fn bind(endpoint: &Path) -> std::io::Result<Listener> {
        if endpoint.exists() {
            // Stale socket: probe first — a live listener refuses removal by
            // simply still being connectable.
            if std::os::unix::net::UnixStream::connect(endpoint).is_ok() {
                return Err(std::io::Error::other("endpoint already served"));
            }
            let _ = std::fs::remove_file(endpoint);
        }
        let parent = endpoint
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        std::fs::create_dir_all(&parent)?;
        let inner = std::os::unix::net::UnixListener::bind(endpoint)?;
        Ok(Listener {
            inner,
            path: endpoint.to_path_buf(),
        })
    }

    pub fn accept(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        let (stream, _) = self.inner.accept()?;
        Ok(stream)
    }

    /// Raw fd for shutdown(2) to wake a blocked accept().
    #[allow(dead_code)] // shutdown(2) wake was replaced by self-connect; kept for U5b
    pub fn raw_fd(&self) -> i32 {
        use std::os::unix::io::AsRawFd;
        self.inner.as_raw_fd()
    }

    #[allow(dead_code)]
    pub fn local_path(&self) -> &Path {
        &self.path
    }
}

/// Peer identity as reported by the OS for one connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeerId {
    pub pid: i64,
    pub uid: Option<u32>,
}

/// Read OS peer credentials (same-uid enforcement happens in auth).
#[cfg(unix)]
pub fn peer_id(stream: &std::os::unix::net::UnixStream) -> std::io::Result<PeerId> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "linux")]
    {
        let mut ucred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut ucred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        return Ok(PeerId {
            pid: ucred.pid as i64,
            uid: Some(ucred.uid),
        });
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    {
        // Darwin/BSD: uid via LOCAL_PEERCRED (xucred), pid via LOCAL_PEERPID.
        let mut xucred: libc::xucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        // SOL_LOCAL (0) with LOCAL_PEERCRED on Darwin/BSD.
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERCRED,
                &mut xucred as *mut libc::xucred as *mut libc::c_void,
                &mut len,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut pid: libc::pid_t = 0;
        let mut pid_len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        let pid_rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                &mut pid as *mut libc::pid_t as *mut libc::c_void,
                &mut pid_len,
            )
        };
        Ok(PeerId {
            pid: if pid_rc == 0 { pid as i64 } else { -1 },
            uid: Some(xucred.cr_uid),
        })
    }
}

#[cfg(windows)]
pub fn peer_id(_stream: &std::os::windows::net::NamedPipeServer) -> std::io::Result<PeerId> {
    // GetNamedPipeClientProcessId supplies the client PID; same-user
    // enforcement rides the pipe's default current-user ACL.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "windows peer credentials land with the windows build (U5b)",
    ))
}

/// Best-effort connect probe used by descriptor staleness checks: an
/// endpoint that ACCEPTS proves a live daemon regardless of pid reuse.
#[cfg(unix)]
pub fn probe(endpoint: &str, timeout: Duration) -> bool {
    use std::os::unix::net::UnixStream;
    let path = Path::new(endpoint);
    if !path.exists() {
        return false;
    }
    match UnixStream::connect(path) {
        Ok(stream) => {
            // Immediately drop; the daemon cleans up the closed connection.
            let _ = stream.set_read_timeout(Some(timeout));
            true
        }
        Err(_) => false,
    }
}

#[cfg(windows)]
pub fn probe(_endpoint: &str, _timeout: Duration) -> bool {
    false
}

// ── windows named-pipe skeleton (compiled only on windows targets) ──────

#[cfg(windows)]
pub mod winpipe {
    //! Minimal named-pipe server mirroring the UDS surface. Compiled only
    //! for windows targets; exercised by the windows leg of the release CI
    //! (U10), not by this repository's darwin test runs.
}
