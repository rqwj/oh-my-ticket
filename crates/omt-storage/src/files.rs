//! Durable file operations for one OMT home: atomic writes
//! (temp-in-same-dir → fsync file → rename over → fsync parent dir),
//! SHA-256 content hashes, recovery copies, directory moves, and the plain
//! [`omt_domain::store::FileStore`] port implementation used by domain-level
//! consumers. Every path is home-relative and contained (R20 seed): `..`
//! components, absolute paths, and empty segments are rejected before any IO.

use crate::{Problem, Result, RECOVERY_ROOT};
use omt_domain::error;
use omt_domain::store::FileStore;
use sha2::{Digest, Sha256};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

/// SHA-256 of UTF-8 content as lowercase hex.
pub fn sha256_hex(content: &str) -> String {
    sha256_hex_bytes(content.as_bytes())
}

/// SHA-256 of raw bytes as lowercase hex (byte-stability witnesses).
pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Validate a home-relative path: forward slashes, no `..`, no leading `/`.
pub fn check_contained(rel_path: &str) -> Result<()> {
    if rel_path.is_empty() {
        return Err(Problem::new(error::IO, "empty home-relative path"));
    }
    if rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return Err(Problem::with_details(
            error::IO,
            "absolute path escapes the home",
            |d| {
                d.insert("path".into(), rel_path.into());
            },
        ));
    }
    for segment in rel_path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(Problem::with_details(
                error::IO,
                "unsafe path segment",
                |d| {
                    d.insert("path".into(), rel_path.into());
                    d.insert("segment".into(), segment.into());
                },
            ));
        }
    }
    Ok(())
}

/// Home-rooted file operator. All relative paths are checked for containment,
/// then resolved under `root`.
pub struct DiskFiles {
    root: PathBuf,
}

impl DiskFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DiskFiles { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve + contain a home-relative path.
    pub fn resolve(&self, rel_path: &str) -> Result<PathBuf> {
        check_contained(rel_path)?;
        Ok(self.root.join(rel_path))
    }

    /// Plain read; missing files are an `IO` problem (domain mapping).
    pub fn read(&self, rel_path: &str) -> Result<String> {
        let path = self.resolve(rel_path)?;
        std::fs::read_to_string(&path).map_err(|err| io_problem(rel_path, &err))
    }

    /// Read a file if it exists (`None` when missing).
    pub fn read_optional(&self, rel_path: &str) -> Result<Option<String>> {
        let path = self.resolve(rel_path)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io_problem(rel_path, &err)),
        }
    }

    /// Hash the current content of a target (`None` when missing).
    pub fn hash_current(&self, rel_path: &str) -> Result<Option<String>> {
        Ok(self
            .read_optional(rel_path)?
            .map(|content| sha256_hex(&content)))
    }

    /// ATOMIC write: temp file in the SAME directory → fsync → rename over →
    /// fsync parent directory. Crash-safe at every boundary; readers never
    /// observe partial content. Returns the after-hash.
    pub fn atomic_write(&self, rel_path: &str, content: &str, token: &str) -> Result<String> {
        let tmp = self.stage_write(rel_path, content, token)?;
        self.fsync_staged(&tmp)?;
        self.promote_staged(&tmp, rel_path)?;
        self.fsync_parent_of(rel_path)?;
        Ok(sha256_hex(content))
    }

    // ── staged write phases (individual kill-point boundaries) ──────────

    /// Phase 1 — write the new content to a hidden temp file in the SAME
    /// directory (rename-over requires same-filesystem). Returns the absolute
    /// temp path.
    pub fn stage_write(&self, rel_path: &str, content: &str, token: &str) -> Result<PathBuf> {
        let path = self.resolve(rel_path)?;
        let parent = path
            .parent()
            .ok_or_else(|| Problem::new(error::IO, format!("no parent dir: {rel_path}")))?;
        std::fs::create_dir_all(parent).map_err(|err| io_problem(rel_path, &err))?;
        let tmp = parent.join(format!(".{}.{}.tmp", file_name_of(rel_path), token));
        let mut file = std::fs::File::create(&tmp).map_err(|err| io_problem(rel_path, &err))?;
        file.write_all(content.as_bytes())
            .map_err(|err| io_problem(rel_path, &err))?;
        Ok(tmp)
    }

    /// Phase 2 — fsync the staged temp file before it becomes visible.
    pub fn fsync_staged(&self, tmp_abs: &Path) -> Result<()> {
        let file = std::fs::File::open(tmp_abs)
            .map_err(|err| Problem::new(error::IO, format!("staged reopen failed: {err}")))?;
        file.sync_all()
            .map_err(|err| Problem::new(error::IO, format!("staged fsync failed: {err}")))
    }

    /// Phase 3 — rename the temp file over its target (cleaning up on failure).
    pub fn promote_staged(&self, tmp_abs: &Path, rel_path: &str) -> Result<()> {
        let path = self.resolve(rel_path)?;
        std::fs::rename(tmp_abs, &path).map_err(|err| {
            let _ = std::fs::remove_file(tmp_abs);
            io_problem(rel_path, &err)
        })
    }

    /// Phase 4 — fsync the parent directory so the rename is durable.
    pub fn fsync_parent_of(&self, rel_path: &str) -> Result<()> {
        let path = self.resolve(rel_path)?;
        match path.parent() {
            Some(parent) => fsync_dir(parent),
            None => Ok(()),
        }
    }

    /// Every `.md`/any file directly or recursively under a relative dir,
    /// sorted (recovery snapshot + drift-gate seeding).
    pub fn list_files_under(&self, rel_dir: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(abs) = self.resolve(rel_dir) {
            walk_all(&abs, rel_dir, &mut out);
        }
        out.sort();
        out
    }

    /// In-place rewrite that PRESERVES the inode (used by home-lock heartbeats:
    /// replacing the inode would silently drop the advisory flock held on it).
    pub fn write_in_place(&self, rel_path: &str, content: &str) -> Result<()> {
        use std::io::{Seek, SeekFrom};
        let path = self.resolve(rel_path)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|err| io_problem(rel_path, &err))?;
        file.seek(SeekFrom::Start(0))
            .map_err(|err| io_problem(rel_path, &err))?;
        file.write_all(content.as_bytes())
            .map_err(|err| io_problem(rel_path, &err))?;
        file.set_len(content.len() as u64)
            .map_err(|err| io_problem(rel_path, &err))?;
        file.sync_all().map_err(|err| io_problem(rel_path, &err))?;
        Ok(())
    }

    /// Copy the current original of one target into the command's recovery
    /// directory (best-effort durable: fsync the copy). Missing originals are
    /// skipped (nothing to restore later).
    pub fn make_recovery_copy(
        &self,
        command_id: &str,
        rel_path: &str,
        recovery_rel_for: &dyn Fn(&str) -> String,
    ) -> Result<()> {
        let Some(original) = self.read_optional(rel_path)? else {
            return Ok(());
        };
        let dest_rel = recovery_rel_for(rel_path);
        // Same-dir temp + rename keeps the copy itself atomic.
        self.atomic_write(&dest_rel, &original, &format!("{command_id}-copy"))?;
        Ok(())
    }

    /// Restore one target from its recovery copy. Returns `false` when no
    /// usable copy exists.
    pub fn restore_from_recovery(
        &self,
        recovery_rel_path: &str,
        target_rel_path: &str,
        token: &str,
    ) -> Result<bool> {
        match self.read_optional(recovery_rel_path)? {
            Some(original) => {
                self.atomic_write(target_rel_path, &original, token)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Move a directory subtree (relative paths), creating the destination
    /// parent. Idempotence is decided by the caller (journal replay checks
    /// presence first).
    pub fn move_dir(&self, from_rel: &str, to_rel: &str, token: &str) -> Result<()> {
        let from = self.resolve(from_rel)?;
        let to = self.resolve(to_rel)?;
        if !from.is_dir() {
            return Err(Problem::with_details(
                error::IO,
                "move source is not a directory",
                |d| {
                    d.insert("from".into(), from_rel.into());
                },
            ));
        }
        if let Some(dest_parent) = to.parent() {
            std::fs::create_dir_all(dest_parent).map_err(|err| io_problem(to_rel, &err))?;
        }
        // Rename within one home stays on one filesystem; fall back to
        // copy+delete only across devices (never observed locally).
        if std::fs::rename(&from, &to).is_ok() {
            fsync_dir(
                to.parent()
                    .ok_or_else(|| Problem::new(error::IO, "no dest parent"))?,
            )?;
            fsync_dir(from.parent().unwrap_or(&self.root))?;
            return Ok(());
        }
        copy_tree(&from, &to, token)?;
        std::fs::remove_dir_all(&from).map_err(|err| io_problem(from_rel, &err))?;
        fsync_dir(to.parent().expect("dest parent"))
    }

    /// Recursively list every `.md` file under `tickets/`, sorted by path —
    /// reindex input order (normative).
    pub fn list_markdown_under_tickets(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk_md(&self.root.join("tickets"), "tickets", &mut out);
        out.sort();
        out
    }

    /// Recovery directory relative path for a command.
    pub fn recovery_dir_rel(command_id: &str) -> String {
        format!("{RECOVERY_ROOT}/{command_id}")
    }

    /// Recovery-copy relative path mirroring a target's relative path.
    pub fn recovery_copy_rel(command_id: &str, target_rel: &str) -> String {
        format!("{}/{}", Self::recovery_dir_rel(command_id), target_rel)
    }

    /// Remove a command's whole recovery directory (acknowledge prune).
    pub fn prune_recovery(&self, command_id: &str) -> Result<()> {
        let dir = self.resolve(Self::recovery_dir_rel(command_id).as_str())?;
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(io_problem(RECOVERY_ROOT, &err)),
        }
    }
}

impl FileStore for DiskFiles {
    fn read_file(&mut self, rel_path: &str) -> Result<String> {
        self.read(rel_path)
    }

    fn write_file(&mut self, rel_path: &str, content: &str) -> Result<()> {
        let token = entropy_token();
        self.atomic_write(rel_path, content, &token).map(|_| ())
    }

    fn move_dir(&mut self, old_rel_dir: &str, new_rel_dir: &str) -> Result<()> {
        let token = entropy_token();
        DiskFiles::move_dir(self, old_rel_dir, new_rel_dir, &token)
    }

    fn delete_file(&mut self, rel_path: &str) -> Result<()> {
        let path = self.resolve(rel_path)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(io_problem(rel_path, &err)),
        }
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.resolve(rel_path).map(|p| p.exists()).unwrap_or(false)
    }

    fn is_dir(&self, rel_path: &str) -> bool {
        self.resolve(rel_path).map(|p| p.is_dir()).unwrap_or(false)
    }

    fn mkdir_all(&mut self, rel_path: &str) -> Result<()> {
        let path = self.resolve(rel_path)?;
        std::fs::create_dir_all(path).map_err(|err| io_problem(rel_path, &err))
    }

    fn list_markdown_under_tickets(&self) -> Vec<String> {
        self.list_markdown_under_tickets()
    }
}

/// Short random token for temp-file names / lock identities (OS entropy).
pub fn entropy_token() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("OS entropy unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Random home id per the schema pattern `^h_[a-z0-9]{6,}$`: `h_` + 12
/// lowercase base-36 chars.
pub fn generate_home_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("OS entropy unavailable");
    let mut id = String::from("h_");
    for byte in bytes {
        id.push(ALPHABET[(byte % ALPHABET.len() as u8) as usize] as char);
    }
    id
}

fn walk_all(abs_dir: &Path, rel_prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(abs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy().to_string();
        let child_rel = format!("{rel_prefix}/{name}");
        if entry.path().is_dir() {
            walk_all(&entry.path(), &child_rel, out);
        } else if !name.starts_with('.') {
            out.push(child_rel);
        }
    }
}

fn walk_md(abs_dir: &Path, rel_prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(abs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let child_rel = format!("{rel_prefix}/{name}");
        if entry.path().is_dir() {
            walk_md(&entry.path(), &child_rel, out);
        } else if name.ends_with(".md") && !name.starts_with('.') {
            out.push(child_rel);
        }
    }
}

fn copy_tree(from: &Path, to: &Path, token: &str) -> Result<()> {
    std::fs::create_dir_all(to)
        .map_err(|err| Problem::new(error::IO, format!("copy_tree: {err}")))?;
    for entry in std::fs::read_dir(from)
        .map_err(|err| Problem::new(error::IO, format!("copy_tree: {err}")))?
    {
        let entry = entry.map_err(|err| Problem::new(error::IO, format!("copy_tree: {err}")))?;
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &dest, token)?;
        } else {
            std::fs::copy(entry.path(), &dest)
                .map_err(|err| Problem::new(error::IO, format!("copy_tree: {err}")))?;
        }
    }
    let _ = token;
    Ok(())
}

/// fsync a directory so renames/creates inside it survive power loss.
#[cfg(unix)]
pub(crate) fn fsync_dir(dir: &Path) -> Result<()> {
    let file = std::fs::File::open(dir)
        .map_err(|err| Problem::new(error::IO, format!("dir open {}: {err}", dir.display())))?;
    file.sync_all()
        .map_err(|err| Problem::new(error::IO, format!("dir fsync: {err}")))
}

#[cfg(not(unix))]
pub(crate) fn fsync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

fn file_name_of(rel_path: &str) -> &str {
    rel_path.rsplit('/').next().unwrap_or(rel_path)
}

fn io_problem(rel_path: &str, err: &std::io::Error) -> Problem {
    Problem::with_details(
        error::IO,
        format!("file operation failed on {rel_path}: {err}"),
        |d| {
            d.insert("path".into(), rel_path.into());
            d.insert("os".into(), err.to_string().into());
        },
    )
}

/// Milliseconds since the platform mtime epoch for one home-relative path —
/// the corrupt-body liveness fallback of the owner-lock reader.
pub fn mtime_ms(files: &DiskFiles, rel_path: &str) -> Option<i64> {
    let path = files.resolve(rel_path).ok()?;
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}
