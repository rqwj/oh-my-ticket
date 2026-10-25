/**
 * §59 P3-1：真正的跨语言前缀锁——此前「锁定」是两边各写一份硬编码
 * （Rust daemon.rs 测试断言自己的字面量、前端 store.ts 另写一份 marker，
 * p55/p57 测试手抄整句文案），没有任何测试从一边读另一边的值：Rust 侧
 * 把前缀和自己的测试一起改，前端会静默把超时退回普通 error。
 * 本 helper 直接读 `../src-tauri/src/daemon.rs` 源码，抽出
 * DAEMON_TIMEOUT_MESSAGE / DAEMON_TIMEOUT_MARKER 字面量供 TS 测试消费
 * ——Rust 侧任何改动（改前缀 / 改文案 / 删常量）都会让消费它的 vitest 红。
 */
import { existsSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'

/** 定位 daemon.rs（vitest 以 apps/desktop 为 cwd；jsdom 下 import.meta.url
 * 不是 file: scheme，不能直接 new URL + readFileSync）。 */
function resolveDaemonRs(): string {
  const viaCwd = resolve(process.cwd(), 'src-tauri/src/daemon.rs')
  if (existsSync(viaCwd)) return viaCwd
  if (import.meta.url.startsWith('file:')) {
    const viaUrl = fileURLToPath(new URL('../src-tauri/src/daemon.rs', import.meta.url))
    if (existsSync(viaUrl)) return viaUrl
  }
  throw new Error('找不到 src-tauri/src/daemon.rs（请在 apps/desktop 目录下运行 vitest）')
}

/** daemon.rs 源码（§59 P3-1 跨语言锁的数据源）。 */
export const DAEMON_TIMEOUT_SOURCE: string = readFileSync(resolveDaemonRs(), 'utf8')

/**
 * 抽 Rust 字符串字面量（值部分）：从 `<decl> =` 之后的开引号扫到闭引号，
 * 处理 `\` 行续（行尾反斜杠 + 换行 + 下一行缩进拼接为空）与 `\"` / `\\` /
 * `\n` 转义。其余转义序列直接红（提示补齐处理，避免静默得到坏值）。
 */
function extractStringLiteral(source: string, decl: string): string {
  const declIdx = source.indexOf(decl)
  if (declIdx === -1) {
    throw new Error(`daemon.rs 里找不到 ${decl}（跨语言锁失效：常量被删/改名）`)
  }
  const eqIdx = source.indexOf('=', declIdx + decl.length)
  const openQuote = eqIdx === -1 ? -1 : source.indexOf('"', eqIdx)
  if (eqIdx === -1 || openQuote === -1) {
    throw new Error(`daemon.rs 的 ${decl} 后找不到字符串字面量（跨语言锁失效）`)
  }
  let out = ''
  let i = openQuote + 1
  while (i < source.length) {
    const ch = source[i]
    if (ch === '\\') {
      const next = source[i + 1] ?? ''
      if (next === '\n') {
        // Rust 行续：反斜杠 + 换行 + 下一行缩进在字面量里拼接为空
        i += 2
        while (i < source.length && (source[i] === ' ' || source[i] === '\t')) i += 1
        continue
      }
      if (next === '"') { out += '"'; i += 2; continue }
      if (next === '\\') { out += '\\'; i += 2; continue }
      if (next === 'n') { out += '\n'; i += 2; continue }
      throw new Error(`daemon.rs 的 ${decl} 含未处理的转义序列 \\${next}（请补齐抽取器）`)
    }
    if (ch === '"') return out
    out += ch
    i += 1
  }
  throw new Error(`daemon.rs 的 ${decl} 字符串字面量未闭合`)
}

/** Rust `DAEMON_TIMEOUT_MESSAGE` 字面量（超时类失败的唯一文案来源）。 */
export function extractDaemonTimeoutMessage(source: string = DAEMON_TIMEOUT_SOURCE): string {
  return extractStringLiteral(source, 'pub const DAEMON_TIMEOUT_MESSAGE: &str')
}

/** Rust `DAEMON_TIMEOUT_MARKER` 字面量（机器前缀，与前端 bridge.ts 同值）。 */
export function extractDaemonTimeoutMarker(source: string = DAEMON_TIMEOUT_SOURCE): string {
  return extractStringLiteral(source, 'pub const DAEMON_TIMEOUT_MARKER: &str')
}
