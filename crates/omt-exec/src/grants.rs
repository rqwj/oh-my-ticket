//! `<runtime>/admin-grants.json` 的加锁读写（P-1）。daemon 每次判定 admin
//! 时都重新读这个文件（auth.rs::admin_principals），所以增删立即生效。
//!
//! 写入方：desktop（单条 `desktop:<pid>` 替换语义）与 exec-shim（释放租约期间
//! 临时登记 `cli:<pid>`，用完即删）。两者经同一把 flock 串行，不会互相覆盖。
//! 临时条目另记在 `exec/temp-grants.json`，启动时清扫 pid 已死的残留。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use omt_harness::atomic_json::{
    lock_exclusive, read_json, update_json, write_atomic, LOCK_ACQUIRE_TIMEOUT,
};

const GRANTS_FILE: &str = "admin-grants.json";

fn grants_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(GRANTS_FILE)
}

fn temp_registry(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("exec").join("temp-grants.json")
}

fn read_principals(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("principalIds")
                .and_then(|ids| ids.as_array())
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// 锁内 read-modify-write；文件形状与 daemon 读取端一致。
fn update_principals(runtime_dir: &Path, f: impl FnOnce(&mut Vec<String>)) -> Result<(), String> {
    let path = grants_path(runtime_dir);
    let _lock = lock_exclusive(&path, LOCK_ACQUIRE_TIMEOUT)?;
    let mut ids = read_principals(&path);
    f(&mut ids);
    let body = serde_json::json!({ "schemaVersion": 1, "principalIds": ids }).to_string();
    write_atomic(&path, &body)
}

/// 当前 principal 列表（测试与诊断用）。
pub fn principals(runtime_dir: &Path) -> Vec<String> {
    read_principals(&grants_path(runtime_dir))
}

/// desktop 语义：删除所有 `prefix` 开头的条目，再加入 `principal`。
pub fn register_replacing_prefix(
    runtime_dir: &Path,
    prefix: &str,
    principal: &str,
) -> Result<(), String> {
    update_principals(runtime_dir, |ids| {
        ids.retain(|id| !id.starts_with(prefix));
        ids.push(principal.to_string());
    })
}

pub fn add(runtime_dir: &Path, principal: &str) -> Result<(), String> {
    update_principals(runtime_dir, |ids| {
        if !ids.iter().any(|id| id == principal) {
            ids.push(principal.to_string());
        }
    })
}

pub fn remove(runtime_dir: &Path, principal: &str) -> Result<(), String> {
    update_principals(runtime_dir, |ids| ids.retain(|id| id != principal))
}

/// §49 临时授权的进程内引用计数：同一进程里多个执行方会共用同一
/// principal（shim 释放租约、desktop 的强制删除 `force_release_agent_leases`
/// 与后台对账 `reconcile_with_connector` 都用 `cli:<本进程 pid>`）——首个
/// 结束的一方 Drop 不得撤掉还在用的一方的授权。按 (runtime 目录, principal)
/// 计数，归零才真正撤销。键用 PathBuf 等值比较（同进程内 runtime 目录来自
/// 同一解析，拼写一致）。
static TEMP_GRANT_REFCOUNTS: std::sync::LazyLock<Mutex<HashMap<(PathBuf, String), usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn refcount_bump(key: &(PathBuf, String)) {
    *TEMP_GRANT_REFCOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(key.clone())
        .or_insert(0) += 1;
}

/// 计数减一；归零（并摘掉表项）返回 true——只有此时才该撤销授权。
fn refcount_release(key: &(PathBuf, String)) -> bool {
    let mut counts = TEMP_GRANT_REFCOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match counts.get_mut(key) {
        Some(count) if *count > 1 => {
            *count -= 1;
            false
        }
        Some(_) => {
            counts.remove(key);
            true
        }
        None => true, // 无计数（防御）：按最后一个持有者处理
    }
}

/// 临时授权守卫：创建时登记（grants + 临时登记表），drop 时撤销。
/// §49：进程内按 principal 引用计数——同 principal 的并发持有者全部 Drop
/// 后才撤销（文件操作保持幂等，重复 add 不叠加登记）。
pub struct TempGrant {
    runtime_dir: PathBuf,
    principal: String,
}

impl TempGrant {
    pub fn add(runtime_dir: &Path, principal: &str) -> Result<Self, String> {
        let key = (runtime_dir.to_path_buf(), principal.to_string());
        refcount_bump(&key);
        let registered = update_json(&temp_registry(runtime_dir), |list: &mut Vec<String>| {
            if list.iter().any(|p| p == principal) {
                return false;
            }
            list.push(principal.to_string());
            true
        });
        let granted = registered.and_then(|_| add(runtime_dir, principal));
        if granted.is_err() {
            // 登记失败：计数回滚（没有守卫会去撤销，不能留下虚高计数）
            refcount_release(&key);
        }
        granted.map(|_| Self {
            runtime_dir: runtime_dir.to_path_buf(),
            principal: principal.to_string(),
        })
    }
}

impl Drop for TempGrant {
    fn drop(&mut self) {
        // §49 refcount：同进程还有别的持有者（强制删除 / 后台对账 / B7 脱离
        // 的 worker 共用 `cli:<pid>`）——不是最后一个，授权保留
        let key = (self.runtime_dir.clone(), self.principal.clone());
        if !refcount_release(&key) {
            return;
        }
        // §39 tempgrant-drop-leak：撤销失败时保留临时登记——启动清扫
        // （sweep_dead_temp_grants）凭它在 pid 死后补删授权。先删登记会让
        // admin 授权永久泄漏（pid 复用即提权窗口）。
        if let Err(err) = remove(&self.runtime_dir, &self.principal) {
            eprintln!(
                "临时授权 {} 撤销失败（{err}），保留登记，留给启动清扫",
                self.principal
            );
            return;
        }
        let _ = update_json(
            &temp_registry(&self.runtime_dir),
            |list: &mut Vec<String>| {
                let before = list.len();
                list.retain(|p| p != &self.principal);
                list.len() != before
            },
        );
    }
}

fn pid_of(principal: &str) -> Option<i64> {
    principal.rsplit(':').next()?.parse().ok()
}

/// 清扫崩溃遗留的临时授权：只处理临时登记表里、pid 已不存在的条目。
pub fn sweep_dead_temp_grants(runtime_dir: &Path) -> Result<usize, String> {
    let (list, _) = read_json::<Vec<String>>(&temp_registry(runtime_dir))?;
    let dead: Vec<String> = list
        .into_iter()
        .filter(|p| {
            pid_of(p)
                .map(|pid| !omt_client::pid_live(pid))
                .unwrap_or(true)
        })
        .collect();
    for principal in &dead {
        remove(runtime_dir, principal)?;
    }
    update_json(&temp_registry(runtime_dir), |list: &mut Vec<String>| {
        let before = list.len();
        list.retain(|p| !dead.contains(p));
        list.len() != before
    })?;
    Ok(dead.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_prefix_keeps_other_principals() {
        let tmp = tempfile::tempdir().unwrap();
        add(tmp.path(), "human:alice").unwrap();
        register_replacing_prefix(tmp.path(), "desktop:", "desktop:100").unwrap();
        register_replacing_prefix(tmp.path(), "desktop:", "desktop:200").unwrap();
        assert_eq!(
            principals(tmp.path()),
            vec!["human:alice".to_string(), "desktop:200".to_string()]
        );
    }

    #[test]
    fn temp_grant_is_removed_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let _g = TempGrant::add(tmp.path(), "cli:4242").unwrap();
            assert!(principals(tmp.path()).contains(&"cli:4242".to_string()));
        }
        assert!(!principals(tmp.path()).contains(&"cli:4242".to_string()));
    }

    /// §49「临时管理员授权无引用计数」：同一进程内并发共用同一 principal
    ///（desktop 的强制删除与后台对账共用 `cli:<pid>`；B7 超时后脱离的 worker
    /// 还会晚些撤销）——先结束的一方 Drop 不得撤掉还在用的一方的授权。
    /// 进程内引用计数，最后一个 Drop 才真正撤销（授权 + 登记表）。
    #[test]
    fn concurrent_temp_grants_refcount_until_the_last_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let first = TempGrant::add(tmp.path(), "cli:7777").unwrap();
        {
            let _second = TempGrant::add(tmp.path(), "cli:7777").unwrap();
            drop(first); // 先结束的一方：不得撤销共用的授权
            assert!(
                principals(tmp.path()).contains(&"cli:7777".to_string()),
                "还有持有者在用，授权必须保留"
            );
            let (registry, _) = read_json::<Vec<String>>(&temp_registry(tmp.path())).unwrap();
            assert!(registry.contains(&"cli:7777".to_string()), "{registry:?}");
        }
        assert!(
            !principals(tmp.path()).contains(&"cli:7777".to_string()),
            "最后一个持有者 Drop 后授权撤销"
        );
        let (registry, _) = read_json::<Vec<String>>(&temp_registry(tmp.path())).unwrap();
        assert!(
            !registry.contains(&"cli:7777".to_string()),
            "登记表同步清理: {registry:?}"
        );
    }

    /// §49 refcount 的回滚臂：登记失败（admin-grants.json 写不进）时计数必须
    /// 回滚——否则下一个同 principal 的 TempGrant 会在计数虚高下提前保活。
    #[test]
    fn failed_temp_grant_add_rolls_the_refcount_back() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path().join("rt");
        std::fs::create_dir_all(rt.join("exec")).unwrap();
        // exec/ 可写（登记表 OK）、根目录只读（admin-grants.json 写失败）
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(TempGrant::add(&rt, "cli:8888").is_err(), "前提：登记失败");
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        // 回滚后：一次 add + drop 仍应完整撤销（计数没有虚高残留）
        {
            let _g = TempGrant::add(&rt, "cli:8888").unwrap();
            assert!(principals(&rt).contains(&"cli:8888".to_string()));
        }
        assert!(!principals(&rt).contains(&"cli:8888".to_string()));
    }

    #[test]
    fn sweep_removes_temp_grants_of_dead_pids_only() {
        let tmp = tempfile::tempdir().unwrap();
        let me = format!("cli:{}", std::process::id());
        // 模拟崩溃遗留：登记了但没有 drop（forget 掉守卫）
        std::mem::forget(TempGrant::add(tmp.path(), &me).unwrap());
        std::mem::forget(TempGrant::add(tmp.path(), "cli:999999").unwrap());
        add(tmp.path(), "cli:999998").unwrap(); // 非临时条目：不归清扫管
        let removed = sweep_dead_temp_grants(tmp.path()).unwrap();
        assert_eq!(removed, 1);
        let now = principals(tmp.path());
        assert!(now.contains(&me));
        assert!(!now.contains(&"cli:999999".to_string()));
        assert!(now.contains(&"cli:999998".to_string()));
    }

    #[test]
    fn concurrent_writers_do_not_lose_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || add(&dir, &format!("p:{i}")).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(principals(&dir).len(), 8);
    }

    /// §39 tempgrant-drop-leak：撤销（admin-grants.json 写回）失败时必须保留
    /// 临时登记——启动清扫凭它在 pid 死后补删授权；此前登记被无条件删除，
    /// 授权从此永久泄漏。手法：runtime 目录只读（admin-grants.json 的 tmp
    /// 建不出来），exec/ 子目录仍可写（登记表在那里）。
    #[test]
    fn failed_revoke_keeps_the_temp_registry_entry() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path().join("rt");
        std::fs::create_dir_all(&rt).unwrap();
        let grant = TempGrant::add(&rt, "cli:999999").unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o555)).unwrap();
        drop(grant);
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            principals(&rt).contains(&"cli:999999".to_string()),
            "前提：撤销确实失败了"
        );
        let (registry, _) = read_json::<Vec<String>>(&temp_registry(&rt)).unwrap();
        assert!(
            registry.contains(&"cli:999999".to_string()),
            "撤销失败时登记必须保留: {registry:?}"
        );
        // 启动清扫据登记补删（999999 不是存活 pid）
        assert_eq!(sweep_dead_temp_grants(&rt).unwrap(), 1);
        assert!(!principals(&rt).contains(&"cli:999999".to_string()));
    }
}
