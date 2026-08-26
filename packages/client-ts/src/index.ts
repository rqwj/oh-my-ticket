/**
 * @omt/client-ts — TypeScript client library for the OMT runtime (plan U5a).
 *
 * Exports the hand-written transport/client layers plus all generated
 * protocol bindings from schema/*.schema.json (single source of truth, R3).
 */
export { Transport, OmtProtocolError } from './transport.js'
export type {
  TransportOptions,
  JsonRpcErrorShape,
  ProblemShape,
} from './transport.js'
export {
  OmtClient,
  type ClientKind,
  type ClientOptions,
  type CredentialInfo,
  type DaemonDescriptor,
  type HandshakeOutcome,
  type RequestedScopes,
} from './client.js'
export {
  DaemonNotFoundError,
  DAEMON_INSTALL_HINT,
  resolveDaemonBinary,
  resetDaemonResolutionCache,
  platformPackageFor,
  daemonSearchPrefixes,
} from './daemon-resolve.js'
export type { DaemonBinary, DaemonSource, ResolveDaemonOptions } from './daemon-resolve.js'
export * from './generated/index.js'
