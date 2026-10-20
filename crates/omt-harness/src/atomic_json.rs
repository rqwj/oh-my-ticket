//! §3.6.6 写纪律的共用实现（F-U15-1 / F-U1-8 / F-U1-9 的汇合点）：
//! 所有机器本地 JSON 状态文件（harness config、agents.json、bindings.json）
//! 统一经由本模块做 read-merge-write。
//!
//! 保证：
//! - **进程内锁**：全局 `STORE_LOCK`，poison 容忍（锁保护的是写互斥而非
//!   不变量，poison 无意义——F-U1-8）；
//! - **跨进程 flock**：companion `<file>.lock`（稳定 inode，rename 不影响
//!   互斥），有界等待后报错而非永久阻塞；
//! - **tmp/rename 原子替换**：tmp 名含 pid + 进程内序号；0600（凭据可能经
//!   env 注入，§3.1 secrets 纪律）；
//! - **残留清扫**：每次 update 顺手清同目录超过 1h 的 `*.tmp-*`（F-U1-9）；
//! - **损坏降级**：解析失败 → 改名 `.corrupt-<ms>` 留证 + 返回默认值 +
//!   `parse_failed` 标志（agents.json 的该标志供 U9 冻结 GC）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 全局串行化：各 store 文件小、写频低，粗粒度锁足够；跨进程并行由 flock
/// 提供。锁保护的是写互斥而非不变量——poison 直接穿透（F-U1-8）。
static STORE_LOCK: Mutex<()> = Mutex::new(());

const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
const TMP_STALE_AFTER: Duration = Duration::from_secs(3600);

/// `update_json` / `read_json` 的附加结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonOutcome {
    /// 本次 update 是否实际落盘（mutate 返回 false 时不写——派生量不落盘）。
    pub changed: bool,
    /// 读取时文件损坏并已降级（agents.json 用它冻结 U9 的 GC）。
    pub parse_failed: bool,
}

fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

fn unique_tmp_path(path: &Path) -> PathBuf {
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    path.with_extension(format!("json.tmp-{}-{seq}", std::process::id()))
}

/// companion lockfile：稳定 inode——数据文件经 rename 原子替换后 inode 会变，
/// 锁必须落在不被替换的路径上。
fn lock_file_of(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    name.push_str(".lock");
    path.with_file_name(name)
}

fn flock_exclusive(file: &std::fs::File, timeout: Duration) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let deadline = Instant::now() + timeout;
    loop {
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::WouldBlock {
            return Err(format!("flock: {err}"));
        }
        if Instant::now() >= deadline {
            return Err(format!("flock: 另一个写入方持有锁超过 {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// unix-only 的 0600/0700 打开模式（v1 macOS-only，§3.6.3）；desktop 各
/// 状态文件与 launcher 共用。
pub trait OpenOptionsUnix {
    fn mode_if_unix(&mut self, mode: u32) -> &mut Self;
}

#[cfg(unix)]
impl OpenOptionsUnix for std::fs::OpenOptions {
    fn mode_if_unix(&mut self, mode: u32) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(mode)
    }
}

#[cfg(not(unix))]
impl OpenOptionsUnix for std::fs::OpenOptions {
    fn mode_if_unix(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

/// 清扫同目录本文件的残留 tmp（写入与 rename 之间崩溃会留下；mtime 超过
/// 阈值才删，避免误删其他进程正在写的 tmp）。
fn sweep_stale_tmp(path: &Path, max_age: Duration) {
    let dir = match path.parent() {
        Some(dir) if dir.is_dir() => dir,
        _ => return,
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!(
        "{}.json.tmp-",
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    );
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .map(|age| age >= max_age)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn write_tmp_and_rename(path: &Path, text: &str) -> Result<(), String> {
    let tmp = unique_tmp_path(path);
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode_if_unix(0o600)
            .open(&tmp)
            .map_err(|e| format!("open tmp: {e}"))?;
        file.write_all(text.as_bytes())
            .map_err(|e| format!("write tmp: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))
}

/// 锁内读取：返回 (值, parse_failed)。文件不存在 → 默认值。
pub fn read_json<T>(path: &Path) -> Result<(T, bool), String>
where
    T: Default + serde::Serialize + serde::de::DeserializeOwned,
{
    let _store_guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    read_json_locked::<T>(path)
}

/// 粘性损坏判定（F-U4a-1 修法 (b)）：同目录存在本文件的 `.corrupt-*` 留证
/// 即 true。`parse_failed` 是瞬态信号——损坏文件被 rename 后第二次读看到
/// 「文件不存在 → 默认值 + parse_failed=false」，U9 据此冻结的 GC 会在
/// 第二次读后失效；粘性判定让「解析失败期间」持续到留证被移走为止（U10
/// reconcile 重建成功后把 `.corrupt-*` 移入 recovered/ 解除）。
pub fn has_unrecovered_corrupt(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        return false;
    }
    let dir = match path.parent() {
        Some(dir) => dir,
        None => return false,
    };
    let prefix = format!("{name}.corrupt-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&prefix))
}

fn read_json_locked<T>(path: &Path) -> Result<(T, bool), String>
where
    T: Default + serde::Serialize + serde::de::DeserializeOwned,
{
    let mut value = T::default();
    let mut parse_failed = false;
    if let Ok(text) = std::fs::read_to_string(path) {
        match serde_json::from_str::<T>(&text) {
            Ok(parsed) => value = parsed,
            Err(err) => {
                parse_failed = true;
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let corrupt = path.with_file_name(format!("{name}.corrupt-{}", epoch_millis()));
                let _ = std::fs::rename(path, &corrupt);
                eprintln!("{} 损坏（{err}），已降级为默认值", path.display());
            }
        }
    }
    Ok((value, parse_failed))
}

/// 锁内 read-merge-write：`mutate` 返回 true 才落盘（仅状态变化写，
/// 派生量不落盘）。跨进程由 companion lockfile 的 flock 互斥。
pub fn update_json<T>(
    path: &Path,
    mutate: impl FnOnce(&mut T) -> bool,
) -> Result<(T, JsonOutcome), String>
where
    T: Default + serde::Serialize + serde::de::DeserializeOwned + PartialEq,
{
    let _store_guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("create_dir_all: {e}"))?;
    }
    sweep_stale_tmp(path, TMP_STALE_AFTER);

    let lock_handle = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode_if_unix(0o600)
        .open(lock_file_of(path))
        .map_err(|e| format!("open lock file: {e}"))?;
    flock_exclusive(&lock_handle, LOCK_ACQUIRE_TIMEOUT)?;

    let (mut value, parse_failed) = read_json_locked::<T>(path)?;
    let changed = mutate(&mut value);
    if changed {
        let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        write_tmp_and_rename(path, &text)?;
    }
    Ok((
        value,
        JsonOutcome {
            changed,
            parse_failed,
        },
    ))
}

/// 进程内单调序号（与 pid 组合生成唯一 tmp/state 后缀）。
pub fn unique_suffix() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{}", std::process::id(), seq)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn update_writes_only_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        let (first, outcome) = update_json::<BTreeMap<String, String>>(&path, |m| {
            m.insert("a".into(), "1".into());
            true
        })
        .unwrap();
        assert!(outcome.changed);
        assert_eq!(first.get("a").map(String::as_str), Some("1"));
        let (_, unchanged) = update_json::<BTreeMap<String, String>>(&path, |_| false).unwrap();
        assert!(!unchanged.changed);
    }

    #[test]
    fn corrupt_file_downgrades_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        std::fs::write(&path, "{ broken").unwrap();
        let (_, outcome) = update_json::<BTreeMap<String, String>>(&path, |_| false).unwrap();
        assert!(outcome.parse_failed);
        let restored: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            restored
                .iter()
                .any(|n| n.starts_with("agents.json.corrupt-")),
            "{restored:?}"
        );
    }

    #[test]
    fn unrecovered_corrupt_is_sticky_and_scoped_per_file() {
        // F-U4a-1 修法 (b)：.corrupt-* 留证存在 → 粘性 true；无留证/无目录
        // → false；判定只看本文件前缀，不跨文件误报。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        assert!(!has_unrecovered_corrupt(&path));

        std::fs::write(&path, "{ broken").unwrap();
        let _ = read_json::<BTreeMap<String, String>>(&path).unwrap();
        assert!(
            has_unrecovered_corrupt(&path),
            "rename 后粘性判定必须仍为 true（第二次读不再翻 false）"
        );
        // 同目录其他文件不受影响
        assert!(!has_unrecovered_corrupt(&dir.path().join("bindings.json")));
    }

    #[test]
    fn tmp_files_are_unique_and_swept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        for _ in 0..3 {
            update_json::<BTreeMap<String, String>>(&path, |m| {
                m.insert("k".into(), "v".into());
                true
            })
            .unwrap();
        }
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".json.tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "rename 后不应残留 tmp: {leftovers:?}");

        // 模拟 crash 残留：手工造一个 tmp，清扫阈值 0 时应被清掉
        let stale = unique_tmp_path(&path);
        std::fs::write(&stale, "junk").unwrap();
        sweep_stale_tmp(&path, Duration::ZERO);
        assert!(!stale.exists());
        // 阈值 1h 时不误删（刚创建的 tmp）
        let fresh = unique_tmp_path(&path);
        std::fs::write(&fresh, "junk").unwrap();
        sweep_stale_tmp(&path, TMP_STALE_AFTER);
        assert!(fresh.exists());
        let _ = std::fs::remove_file(&fresh);
    }

    #[cfg(unix)]
    #[test]
    fn written_file_has_0600_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        update_json::<BTreeMap<String, String>>(&path, |m| {
            m.insert("a".into(), "1".into());
            true
        })
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn companion_lock_is_exclusive_across_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        let lock_path = lock_file_of(&path);
        let first = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        flock_exclusive(&first, Duration::from_millis(100)).unwrap();
        // 另一个 fd（模拟另一进程/并发写入方）应拿不到锁
        let second = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        assert!(flock_exclusive(&second, Duration::from_millis(150)).is_err());
        drop(first);
        // 释放后可再获取
        flock_exclusive(&second, Duration::from_millis(100)).unwrap();
    }

    #[test]
    fn concurrent_update_json_does_not_lose_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.json");
        std::thread::scope(|scope| {
            for i in 0..8u32 {
                let path = path.clone();
                scope.spawn(move || {
                    for _ in 0..5 {
                        update_json::<BTreeMap<String, String>>(&path, |m| {
                            m.insert(format!("k{i}"), "v".into());
                            true
                        })
                        .unwrap();
                    }
                });
            }
        });
        let (map, _) = read_json::<BTreeMap<String, String>>(&path).unwrap();
        assert_eq!(map.len(), 8, "{map:?}");
    }
}
