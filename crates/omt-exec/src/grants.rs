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
/// 计数，归零才真正撤销。键的 runtime 目录先 canonicalize（§51 F-SP2-3 顺带）：
/// 同一目录的不同拼写（如 macOS 的 /var 与 /private/var）不得拆成两个计数键。
///
/// §51 F-SP2-3：`add` 的「计数 +1 + 登记/授权」与 `Drop` 的「计数 −1（归零时
/// 撤销）」必须在**同一把互斥锁内**完成——此前归零与 `remove()` 之间会插进
/// 另一方的 `add`（见到授权仍在而不重登记），随后归零方的 `remove()` 把它
/// 持有的授权一并撤掉。文件操作都是本地短 IO，锁内完成可接受；锁序恒为
/// 「计数锁 → 文件 flock」，无反向路径。
static TEMP_GRANT_REFCOUNTS: std::sync::LazyLock<Mutex<HashMap<(PathBuf, String), usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// 计数键的 runtime 目录规范化：canonicalize（失败回退原拼写——目录尚在
/// 创建前等场景）。守卫内存的是规范化后的路径，Drop 与 add 的键天然一致。
fn normalized_runtime_dir(runtime_dir: &Path) -> PathBuf {
    std::fs::canonicalize(runtime_dir).unwrap_or_else(|_| runtime_dir.to_path_buf())
}

fn lock_counts() -> std::sync::MutexGuard<'static, HashMap<(PathBuf, String), usize>> {
    TEMP_GRANT_REFCOUNTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn refcount_bump(counts: &mut HashMap<(PathBuf, String), usize>, key: &(PathBuf, String)) {
    *counts.entry(key.clone()).or_insert(0) += 1;
}

/// 计数减一；归零（并摘掉表项）返回 true——只有此时才该撤销授权。
fn refcount_release(
    counts: &mut HashMap<(PathBuf, String), usize>,
    key: &(PathBuf, String),
) -> bool {
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

/// §51 F-SP2-3 交错测试缝：最后一个持有者进入撤销段前挂起一段时间，
/// 把「归零 → 撤销」的竞态窗口放大成确定长度。仅测试编译；生产恒为 no-op。
#[cfg(test)]
static TEST_REVOKE_DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
fn test_revoke_delay() {
    let ms = TEST_REVOKE_DELAY_MS.load(std::sync::atomic::Ordering::Relaxed);
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

/// 临时授权守卫：创建时登记（grants + 临时登记表），drop 时撤销。
/// §49：进程内按 principal 引用计数——同 principal 的并发持有者全部 Drop
/// 后才撤销（文件操作保持幂等，重复 add 不叠加登记）。§51 F-SP2-3：计数
/// 与文件操作同锁完成（见 TEMP_GRANT_REFCOUNTS 的说明）。
pub struct TempGrant {
    runtime_dir: PathBuf,
    principal: String,
}

impl TempGrant {
    pub fn add(runtime_dir: &Path, principal: &str) -> Result<Self, String> {
        let runtime_dir = normalized_runtime_dir(runtime_dir);
        let key = (runtime_dir.clone(), principal.to_string());
        let mut counts = lock_counts();
        refcount_bump(&mut counts, &key);
        let registered = update_json(&temp_registry(&runtime_dir), |list: &mut Vec<String>| {
            if list.iter().any(|p| p == principal) {
                return false;
            }
            list.push(principal.to_string());
            true
        });
        let granted = registered.and_then(|_| add(&runtime_dir, principal));
        if granted.is_err() {
            // 登记失败：计数回滚（没有守卫会去撤销，不能留下虚高计数）
            refcount_release(&mut counts, &key);
        }
        granted.map(|_| Self {
            runtime_dir,
            principal: principal.to_string(),
        })
    }
}

impl Drop for TempGrant {
    fn drop(&mut self) {
        // §49 refcount：同进程还有别的持有者（强制删除 / 后台对账 / B7 脱离
        // 的 worker 共用 `cli:<pid>`）——不是最后一个，授权保留
        let key = (self.runtime_dir.clone(), self.principal.clone());
        let mut counts = lock_counts();
        if !refcount_release(&mut counts, &key) {
            return;
        }
        // §51 F-SP2-3 测试缝：在仍持有计数锁的撤销段起点挂起，见上
        #[cfg(test)]
        test_revoke_delay();
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

    /// §51 F-SP2-3「归零 → 撤销」与新 acquire 的交错：A 的 Drop 已把计数
    /// 归零、正在撤销的窗口里，B 的 add 不得在窗口内「见到授权仍在」就
    /// no-op——否则 A 随后的 remove() 会把 B 持有的授权一并撤掉。
    /// 确定性手法：TEST_REVOKE_DELAY_MS 让 A 的撤销段挂起固定时长，期间
    /// B 完成 add；判定点在 A 的 Drop 结束之后（带界轮询等 B 的登记落盘）。
    /// 旧实现（计数与文件操作分离）在本测下 A 的 remove 必然落在 B 的
    /// add 之后——B 持守卫而授权已被撤，红。
    #[test]
    fn temp_grant_zero_release_interleaved_with_add_keeps_authorization() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path().join("rt");
        std::fs::create_dir_all(rt.join("exec")).unwrap();
        let a = TempGrant::add(&rt, "cli:777").unwrap();
        assert!(principals(&rt).contains(&"cli:777".to_string()));
        TEST_REVOKE_DELAY_MS.store(50, std::sync::atomic::Ordering::Relaxed);
        let dropper = std::thread::spawn(move || drop(a));
        // 让 A 进入撤销段（归零后的延迟窗口内）
        std::thread::sleep(std::time::Duration::from_millis(10));
        let b = TempGrant::add(&rt, "cli:777").unwrap();
        dropper.join().unwrap();
        TEST_REVOKE_DELAY_MS.store(0, std::sync::atomic::Ordering::Relaxed);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !principals(&rt).contains(&"cli:777".to_string()) {
            assert!(
                std::time::Instant::now() < deadline,
                "B 仍持有守卫，但授权被 A 的撤销带走（§51 F-SP2-3 交错）"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let (registry, _) = read_json::<Vec<String>>(&temp_registry(&rt)).unwrap();
        assert!(registry.contains(&"cli:777".to_string()), "{registry:?}");
        drop(b);
        assert!(!principals(&rt).contains(&"cli:777".to_string()));
        let (registry, _) = read_json::<Vec<String>>(&temp_registry(&rt)).unwrap();
        assert!(!registry.contains(&"cli:777".to_string()), "{registry:?}");
    }

    /// §51 F-SP2-3 顺带：计数键按 canonicalize 后的 runtime 目录——同一
    /// 目录的两种拼写（macOS 上 /var 与 /private/var）是同一把计数。
    #[test]
    fn temp_grant_refcount_key_is_canonicalized() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = tmp.path().join("rt");
        std::fs::create_dir_all(&rt).unwrap();
        let alias = {
            // macOS 的 $TMPDIR 形如 /var/folders/...，canonicalize 后是
            // /private/var/...——构造出同一目录的未规范化拼写
            let canonical = std::fs::canonicalize(&rt).unwrap();
            let raw = rt.display().to_string();
            if canonical.display().to_string() == raw {
                // 环境给的就是规范形态：跳过（不是本测的目标平台形态）
                return;
            }
            std::path::PathBuf::from(raw)
        };
        let a = TempGrant::add(&rt, "cli:7000").unwrap();
        {
            // 经别名拼写 add：旧键语义下这是「另一个 runtime 目录」的独立
            // 计数，drop 时会立即撤销；规范化后应计 2，drop 不撤销
            let _b = TempGrant::add(&alias, "cli:7000").unwrap();
            drop(a);
            assert!(
                principals(&rt).contains(&"cli:7000".to_string()),
                "别名拼写的 add 必须并入同一计数——还有持有者在用"
            );
        }
        assert!(!principals(&rt).contains(&"cli:7000".to_string()));
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
