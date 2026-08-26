/**
 * Daemon binary resolution (plan U13, R21/KD1 — KTD7): THE shared resolver
 * every consumption layer goes through before spawning `omt-daemon`.
 *
 * Resolution precedence (first hit wins; v1 does no semver comparison):
 *
 *   1. explicit option (`daemonPath`) / `OMT_DAEMON` env — highest;
 *   2. system PATH lookup for `omt-daemon`;
 *   3. known install prefixes (`~/.local/bin`, `/opt/homebrew/bin`,
 *      `/usr/local/bin` — where brew / install.sh put the product channel);
 *   4. installed npm platform package
 *      `@oh-my-ticket/<triple>/bin/omt-daemon` (require.resolve fallback,
 *      triple derived from process.platform/process.arch);
 *   5. nothing → {@link DaemonNotFoundError} with updated install guidance
 *      (product channels first: brew tap / install.sh one-liner; npm
 *      platform packages are a fallback, KD1).
 *
 * The first non-explicit hit is memoized at module level: resolution runs on
 * every discover-or-spawn (including reconnects), and re-scanning after a
 * hit can only churn (binary upgraded mid-session changes nothing until the
 * process restarts). Explicit options bypass the memo — caller intent always
 * wins over a cached guess.
 *
 * esbuild/bundler safety: the platform-package lookup uses
 * `createRequire(import.meta.url)` with a dynamically concatenated specifier,
 * so no bundler tries to statically resolve `@oh-my-ticket/*` at build time;
 * the createRequire anchor also makes the fallback resolve against whatever
 * node_modules the SHIPPED package sits in (zero-config for DSH users).
 */
import { accessSync, constants as fsConstants, existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { homedir } from 'node:os'
import { join } from 'node:path'

/** Where a resolved daemon binary came from (diagnostics + tests). */
export type DaemonSource = 'explicit' | 'env' | 'path' | 'platform-package'

export interface DaemonBinary {
  /** Absolute or caller-supplied path passable to spawn(). */
  readonly path: string
  /** Which precedence level produced the hit. */
  readonly source: DaemonSource
}

export interface ResolveDaemonOptions {
  /** Explicit override (adapter `daemonPath` option); beats everything. */
  readonly explicit?: string
}

/** Binary name looked up on PATH and under known prefixes. */
export const DAEMON_BINARY_NAME = 'omt-daemon'

/**
 * npm scope packages carrying per-triple binaries (R21). Directory names
 * match npm/platform-packages/ templates 1:1 and are filled by release.yml
 * job3 from Release artifacts.
 */
const PLATFORM_TRIPLES: ReadonlyMap<string, string> = new Map([
  ['darwin-arm64', '@oh-my-ticket/darwin-arm64'],
  ['darwin-x64', '@oh-my-ticket/darwin-x64'],
  ['linux-arm64', '@oh-my-ticket/linux-arm64'],
  ['linux-x64', '@oh-my-ticket/linux-x64'],
])

/**
 * The platform package matching THIS runtime, or null when no npm fallback
 * exists for the pair (Windows is unsupported this release, R23).
 */
export function platformPackageFor(
  platform: string = process.platform,
  arch: string = process.arch,
): string | null {
  return PLATFORM_TRIPLES.get(`${platform}-${arch}`) ?? null
}

/**
 * Known install prefixes probed after PATH (KTD7 level 3): the directories
 * the product channels install into — install.sh defaults to ~/.local/bin,
 * Homebrew on Apple silicon uses /opt/homebrew/bin, Intel/legacy uses
 * /usr/local/bin.
 */
export function daemonSearchPrefixes(home: string = homedir()): string[] {
  return [join(home, '.local', 'bin'), '/opt/homebrew/bin', '/usr/local/bin']
}

/** Install guidance carried by the exhausted-resolution error (KD1 order:
 * product channels first, npm platform packages as fallback). */
export const DAEMON_INSTALL_HINT = [
  'install the omt runtime via a product channel:',
  '  brew tap rqwj/omt https://github.com/rqwj/homebrew-omt && brew install omt',
  '  curl -fsSL https://raw.githubusercontent.com/rqwj/oh-my-ticket/main/scripts/install.sh | sh',
  'fallback: npm install -g @oh-my-ticket/<platform-arch> (e.g. @oh-my-ticket/darwin-arm64)',
  'or set OMT_DAEMON to an existing omt-daemon binary path',
].join('\n')

/**
 * Thrown when every precedence level misses. `attempts` records what was
 * probed (diagnostics); `hint` carries the actionable install guidance.
 */
export class DaemonNotFoundError extends Error {
  readonly code = 'OMT_DAEMON_NOT_FOUND'
  readonly attempts: readonly string[]
  readonly hint: string

  constructor(attempts: readonly string[], hint: string = DAEMON_INSTALL_HINT) {
    super(`omt-daemon not found (searched: ${attempts.join('; ')}).\n${hint}`)
    this.name = 'DaemonNotFoundError'
    this.attempts = attempts
    this.hint = hint
  }
}

// Module-level memo of the first non-explicit hit (see module docstring).
let cached: DaemonBinary | undefined

/** Forget the memoized resolution (tests / smoke harnesses only). */
export function resetDaemonResolutionCache(): void {
  cached = undefined
}

function isExecutableFile(path: string): boolean {
  if (!existsSync(path)) return false
  try {
    accessSync(path, fsConstants.X_OK)
    return true
  } catch {
    return false
  }
}

/** Scan $PATH for an executable `omt-daemon`; first hit wins. */
function findOnPath(pathEnv: string | undefined, attempts: string[]): string | null {
  const dirs = (pathEnv ?? '').split(process.platform === 'win32' ? ';' : ':').filter(dir => dir.trim() !== '')
  for (const dir of dirs) {
    const candidate = join(dir, DAEMON_BINARY_NAME)
    if (isExecutableFile(candidate)) return candidate
  }
  attempts.push(`PATH lookup (${dirs.length > 0 ? `${dirs.length} dirs, no ${DAEMON_BINARY_NAME}` : 'empty'})`)
  return null
}

/**
 * Resolve the daemon binary to spawn, following KTD7 precedence. Throws
 * {@link DaemonNotFoundError} when everything misses. Synchronous: only
 * filesystem probes and require.resolve — cheap enough for the hot path,
 * memoized after the first non-explicit hit.
 */
export function resolveDaemonBinary(opts: ResolveDaemonOptions = {}): DaemonBinary {
  // Level 1a: explicit option — deterministic, bypasses the memo.
  const explicit = opts.explicit?.trim()
  if (explicit !== undefined && explicit !== '') {
    return { path: explicit, source: 'explicit' }
  }

  // Level 1b: OMT_DAEMON env (same tier per R21; labeled separately).
  const envPath = process.env.OMT_DAEMON?.trim()
  if (envPath !== undefined && envPath !== '') {
    cached ??= { path: envPath, source: 'env' }
    return cached
  }

  if (cached !== undefined) return cached

  const attempts: string[] = [`explicit option (${opts.explicit === undefined ? 'absent' : 'empty'})`, 'OMT_DAEMON env (unset)']

  // Level 2: system PATH.
  const fromPath = findOnPath(process.env.PATH, attempts)
  if (fromPath !== null) {
    cached = { path: fromPath, source: 'path' }
    return cached
  }

  // Level 3: known prefixes (brew / install.sh product-channel locations).
  for (const prefix of daemonSearchPrefixes()) {
    const candidate = join(prefix, DAEMON_BINARY_NAME)
    if (isExecutableFile(candidate)) {
      cached = { path: candidate, source: 'path' }
      return cached
    }
  }
  attempts.push(`known prefixes (~/.local/bin, /opt/homebrew/bin, /usr/local/bin: no ${DAEMON_BINARY_NAME})`)

  // Level 4: installed npm platform package (DSH adapter's internal fallback,
  // KD1). resolve() throws when absent; the dynamically-built specifier keeps
  // bundlers from trying to inline @oh-my-ticket/*.
  const pkg = platformPackageFor()
  if (pkg === null) {
    attempts.push(`npm platform package (none exists for ${process.platform}-${process.arch}; unsupported this release)`)
  } else {
    try {
      const requireFromHere = createRequire(import.meta.url)
      const resolved = requireFromHere.resolve(`${pkg}/bin/${DAEMON_BINARY_NAME}`) as string
      cached = { path: resolved, source: 'platform-package' }
      return cached
    } catch {
      attempts.push(`npm platform package ${pkg} (not installed)`)
    }
  }

  throw new DaemonNotFoundError(attempts)
}
