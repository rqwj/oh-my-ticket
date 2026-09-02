/**
 * Workspace 显示路径：home 的持久键是 <root>/.omt 目录，但用户心智里
 * 的 workspace 是它的 ROOT。显示规则：剥掉末尾 '/.omt'；homeDir 前缀
 * 压缩为 '~'（homeDir 由 Rust 侧随 daemon_homes 下发）。全局 home
 * （~/.omt）因此显示为 '~'。
 */
export function workspaceRootOf(path: string | undefined, homeDir: string | undefined, fallback: string): string {
  if (path === undefined || path === '') return fallback
  let root = path.replace(/\/+$/, '')
  if (root.endsWith('/.omt')) root = root.slice(0, -'/.omt'.length)
  else if (root === '.omt') root = homeDir ?? '~'
  if (homeDir && (root === homeDir || root.startsWith(`${homeDir}/`))) {
    root = `~${root.slice(homeDir.length)}`
  }
  return root === '' ? fallback : root
}
