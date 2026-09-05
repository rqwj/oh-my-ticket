---
title: "fix: Renew rejected OMT credentials safely"
date: 2026-09-05
type: fix
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: ce-plan-bootstrap
execution: code
---

## Goal Capsule
Restore long-lived client calls after daemon credential expiry without reconnecting healthy sockets or replaying uncertain writes.
Only code and PR delivery are authorized; running Web, installed plugins, and deployment configuration remain untouched.

## Product Contract
- R1. Recover once only from UNAUTHORIZED with reason unknown-credential or expired-credential, which the server rejects before method dispatch.
- R2. Preserve enrollment kind, requested scopes, name and session identity, active subscriptions and unrelated pending calls.
- R3. Coalesce concurrent/late failures for the same transport and rejected credential; discard late renewal results after close or transport replacement; failed renewal remains retryable on a subsequent call.
- R4. Rebuild authenticated parameters following either renewal or existing opt-in home-scope reconnect. Keep that opt-in disabled by default; never retry ordinary FORBIDDEN, missing credentials or uncertain network failures as credential recovery.

## Planning Contract
The registry in `crates/omt-runtime/src/auth.rs` lazily evicts expired credentials, reporting unknown-credential. `crates/omt-runtime/src/server.rs` rejects these before route_method; `packages/client-ts/src/client.ts` caches the handshake and lacks recovery.
Same-socket enrollment is preferred over forceReconnect because reconnect rejects unrelated pending calls whose writes may still execute server-side.
No credential TTL, permission policy or server protocol changes are required.

Directional lifecycle: authenticated call -> eligible rejection -> shared same-socket enrollment -> verify original transport still current -> publish fresh credential -> retry once with rebuilt parameters. Other errors terminate. A late rejection after successful renewal reuses the current credential; close/replacement makes old renewal ineligible to publish.

## Implementation Units
### U1. Safe credential recovery
Covers R1-R4. Update `packages/client-ts/src/client.ts` and add focused socket regression coverage in `tests/credential-renewal.spec.ts`, following `tests/reconnect.spec.ts` and `tests/rpc.spec.ts`.
Execution note: demonstrate the missing recovery with a failing regression before implementation, then verify the corrected behavior.
This is a single outcome; test and implementation changes share state and are sequential, not separate parallel tickets.

## Verification Contract
- Simulate unknown/expired credential responses without waiting 12 hours; verify one enrollment, original enrollment arguments, fresh token retry and same socket.
- Concurrent and delayed failures share renewal; a second authorization rejection stops; failed enrollment does not poison later attempts.
- Close/replacement during renewal cannot publish stale results or send a retry on a dead transport.
- Unrelated pending writes and subscription delivery survive renewal; reconnect.enabled=false still permits same-socket renewal.
- Ineligible authorization and network failures receive no blind replay; home-scope opt-in behavior remains unchanged and its retry uses the new token.
Run focused TypeScript tests, client typecheck and broader applicable suites; report any baseline or environment failures accurately.

## Definition of Done
Regression fails before and passes after the minimal fix; tests and review have no unresolved blocking defects. Commit only task changes on a new branch from current origin/main and open a PR. Report base/head/commit, test evidence, OMT IDs and isolated worktree path.
