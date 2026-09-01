/**
 * Host-layer facade for daemon binary resolution (plan U13, R21/KD1 —
 * KTD7). The shared resolver implementation lives in @omt/client-ts (the
 * spawn choke point both layers consume); this module is the DSH adapter's
 * entry point and owns the mapping onto the adapter's OmtError contract:
 * an exhausted search surfaces as IO/runtime-unavailable with the UPDATED
 * install guidance (product channels brew/install.sh first, npm platform
 * packages as fallback) instead of a raw stack.
 */
import { OmtError, type ProblemDetails } from '../types.ts'
import { DaemonNotFoundError } from '@omt/client-ts'

export {
  DAEMON_INSTALL_HINT,
  DaemonNotFoundError,
  daemonSearchPrefixes,
  platformPackageFor,
  resetDaemonResolutionCache,
  resolveDaemonBinary,
} from '@omt/client-ts'
export type { DaemonBinary, DaemonSource, ResolveDaemonOptions } from '@omt/client-ts'

/**
 * Map a failed daemon resolution onto OmtError('IO', …,
 * reason:"runtime-unavailable"), preserving the structured attempts and the
 * install hint. Returns undefined for any other error so callers keep their
 * existing error contract.
 */
export function runtimeUnavailableFromResolution(error: unknown): OmtError | undefined {
  if (!(error instanceof Error)) return undefined
  // instanceof covers in-process throws; name/code also match copies that
  // crossed a bundle boundary where the class identity differs.
  const isResolutionMiss =
    error instanceof DaemonNotFoundError ||
    error.name === 'DaemonNotFoundError' ||
    (error as NodeJS.ErrnoException).code === 'OMT_DAEMON_NOT_FOUND'
  if (!isResolutionMiss) return undefined
  const details: ProblemDetails = {
    reason: 'runtime-unavailable',
    ...(error instanceof DaemonNotFoundError ? { attempts: [...error.attempts] } : {}),
    hint: error instanceof DaemonNotFoundError ? error.hint : error.message,
  }
  return new OmtError('IO', `OMT runtime unavailable: ${error.message}`, details)
}
