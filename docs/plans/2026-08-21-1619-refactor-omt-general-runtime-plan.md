---
title: OMT General Runtime - Plan
type: refactor
date: 2026-08-21
deepened: 2026-08-21
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: ce-plan-bootstrap
execution: code
---

# OMT General Runtime - Plan

## Goal Capsule

- **Objective:** Turn OMT from a DeepSeek Harness plugin with an embedded data core into a single-writer local runtime that multiple harnesses can call safely.
- **Authority hierarchy:** This plan and its R-IDs define the target behavior; existing node/run semantics in `src/host/core.ts` remain compatibility evidence; existing DSH UX and tool names remain the migration baseline.
- **Execution profile:** Deep, cross-cutting refactor across contracts, persistence, process topology, transports, packaging, and DSH integration.
- **Stop conditions:** Stop before implementation if Markdown can no longer remain Git-friendly and hand-editable, existing `.omt` homes cannot migrate without content loss, or the rollout cannot establish a quiescent handoff from legacy writers to the daemon.
- **Tail ownership:** The implementation owns package migration, compatibility adapters, data migration, crash recovery, reference-harness proof, DSH regression proof, and release documentation.

---

## Product Contract

### Summary

OMT becomes a local, single-writer runtime with a versioned command/query/event contract. DSH remains a first-class adapter with its current UI and model-facing names, while other harnesses integrate through MCP or the runtime client SDK. The first release does not add a generic Web UI or a hosted multi-tenant service.

### Problem Frame

The current package is deployed as one DSH bundle. `src/index.ts` assembles storage, tools, RPC, skills, browser push, and agent lifecycle hooks in one Cordis plugin. `src/host/core.ts` is reusable only in appearance: it combines domain rules, SQLite operations, Markdown writes, event emission, run orchestration, and session-liveness behavior.

This shape works while one DSH process owns a home. It is unsafe as a multi-harness runtime. Most mutations are multi-step read/check/write flows, file writes use direct `writeFile`, and only run claim uses `BEGIN IMMEDIATE`. Two processes can race IDs, overwrite Markdown, claim after a pause, or split run-item state from ticket state. The in-memory `ChangeHub`, DSH session IDs, `agent/status`, `followup`, and `inject` also cannot serve as a portable execution protocol.

Publishing `OmtCore` directly would freeze these couplings as public API. The refactor must first establish stable contracts, a recoverable single-writer storage boundary, and host-neutral executor semantics.

### Key Decisions

- **A local runtime service is the product boundary.** `(session-settled: user-approved — chosen over a pure embedded SDK: several harness processes must not write the same home independently.)` Governs R1, R2, R7, R8, R9.
- **MCP is an adapter, not the sole internal protocol.** `(session-settled: user-approved — chosen over MCP as the only protocol: DSH UI, resumable events, executor leases, and administration need a richer stable contract.)` Governs R3, R11, R13, R14.
- **Preserve the DSH product surface before adding a generic UI.** `(session-settled: user-approved — chosen over including a generic Web UI in the first release: runtime correctness and compatibility are the critical path.)` Governs R13, R18.

### Actors

- A1. **OMT user:** Creates and edits ticket trees, executes runs, hand-edits Markdown, and expects project-local data to remain Git-friendly.
- A2. **DSH adapter:** Preserves `omt_*` tools, skills, session notifications, RPC endpoints, SSE refresh, and the existing browser UI.
- A3. **External harness adapter:** Calls OMT through MCP or the runtime SDK and maps its own actor/session lifecycle to runtime leases and events.
- A4. **OMT runtime daemon:** Owns home discovery, write serialization, migrations, command handling, durable events, leases, and crash recovery.

### Requirements

#### Runtime boundary and contracts

- R1. The domain and application runtime must not import Cordis, DSH services, React, browser globals, or DSH session types.
- R2. One daemon must be the canonical writer for every opened OMT home, and a home lock must reject concurrent legacy or second-daemon writers.
- R3. Commands, queries, events, errors, handshake data, and capabilities must use one versioned schema source shared by all transports and adapters.
- R4. Public node and run references must include a stable `homeId`; bare IDs remain a DSH compatibility input resolved only with an explicit workspace context.
- R5. Public errors must carry stable machine-readable codes and details; message text is diagnostic and not a compatibility contract.

#### Persistence and recovery

- R6. Existing Markdown layout, user-authored bodies, managed child blocks, workspace `.omt` directories, global homes, and explicit reindex behavior must remain supported.
- R7. A mutation may return success only after its SQLite state, idempotent result, durable event set, and file recovery conditions are committed; an acknowledged mutation must recover by roll-forward and must never revert to prior state.
- R8. Database migrations must use a ledger, run under an exclusive writer, reject newer unsupported schemas before any write-capable open step, verify invariants, and preserve a restorable DB-plus-Markdown snapshot of existing v1-v3 homes.
- R9. Mutating commands must support idempotency and optimistic revision checks so transport retries and concurrent clients do not duplicate or overwrite acknowledged changes.

#### Runs, executors, and events

- R10. Claim must atomically validate run state, assign an executor lease, and fence report/renew/release operations by lease token and attempt; only a separately authorized and audited human-administration capability may confirm or override another executor's item.
- R11. Runtime events must have durable per-home cursors and replay so adapters can reconnect without relying on an in-memory counter; attention events must identify the required action and affected reference without exposing lease secrets.
- R12. Runtime identity must distinguish connection principal, delegated actor namespace, and administrator capability; no adapter may submit an unrestricted actor or session ID.

#### Harness integration

- R13. The DSH adapter must preserve existing `omt_*` tool names, embedded skills, workspace routing, current browser UI, `/omt` RPC behavior, SSE refresh, and followup/inject semantics while delegating all runtime operations to the new client and publishing a process-level Cordis service for other DSH plugins.
- R14. The MCP adapter must expose every portable action classified as available to agents, with structured outputs, stable errors, capability discovery, and stdio plus optional Streamable HTTP entry points.
- R15. A minimal non-DSH reference harness must prove node CRUD, run claim/report, event replay, attention handling, and multi-client access against the same daemon.

#### Packaging and compatibility

- R16. Contracts, runtime, storage, client, server, MCP adapter, and DSH adapter must be separately buildable packages with declarations and without requiring a DSH checkout outside the DSH package.
- R17. Existing `oh-my-ticket` installations and `.omt` homes must have a documented migration path that preserves package identity, model tool names, ticket IDs, Markdown paths, and run records.
- R18. The first general-runtime release must document supported Node, OS, protocol, schema, DSH, and MCP versions and must not claim generic UI or remote multi-tenant support.

#### Cross-cutting safety and parity

- R19. Reindex must produce a deterministic dry-run plan for missing or invalid nodes; missing active run members are quarantined and interrupted with their identity snapshot preserved, and no member is silently deleted or rebound.
- R20. Every principal must have explicit home and operation capabilities; home paths must be canonical, contained by configured roots, and protected against traversal, symlink, junction, hardlink, and path-replacement escapes.
- R21. The daemon must enforce bounded request, connection, home, queue, search, reindex, event, idempotency, log, and disk-retention limits so one local client or home cannot exhaust the runtime.
- R22. An action-parity matrix must classify each UI, DSH tool, runtime, and MCP action as agent-available, adapter-only, or human-administrative; all agent-available actions must expose the same qualified home context, revision, run state, and recovery guidance.

### Key Flows

- F1. **Daemon discovery and connection**
  - **Trigger:** A harness starts or connects to OMT for a workspace.
  - **Actors:** A2 or A3, A4.
  - **Steps:** The client discovers or starts the local daemon, negotiates protocol/capabilities, authenticates, resolves workspace/global home, and receives the stable `homeId`.
  - **Outcome:** All callers address the same writer and use qualified references.
  - **Covered by:** R2, R3, R4, R12.
- F2. **Recoverable mutation**
  - **Trigger:** A client creates, updates, moves, or reports a ticket/run change.
  - **Actors:** A2 or A3, A4.
  - **Steps:** The daemon validates context and revisions, records an idempotent operation, applies atomic file changes, commits SQLite/index/event state, and acknowledges only after recovery data is durable.
  - **Outcome:** A retry returns the same result, and restart recovery converges the home.
  - **Covered by:** R5, R7, R8, R9, R11.
- F3. **Leased run execution**
  - **Trigger:** A harness claims the next pending run item.
  - **Actors:** A2 or A3, A4.
  - **Steps:** The daemon validates the run and concurrency policy inside one transaction, assigns a lease and attempt, accepts heartbeats, and accepts report only from the fenced lease or an explicit administrative confirmation path.
  - **Outcome:** Pause/cancel cannot race into a late claim, and stale executors cannot complete a newer attempt.
  - **Covered by:** R10, R12.
- F4. **Event delivery and host-specific action**
  - **Trigger:** A committed operation emits a node, run, lease, or attention event.
  - **Actors:** A4, A2 or A3.
  - **Steps:** The adapter resumes from its cursor, acknowledges delivery state, refreshes its views, and maps attention events to its own followup, notification, or polling mechanism.
  - **Outcome:** Runtime correctness does not depend on DSH events, but DSH keeps its current interaction quality.
  - **Covered by:** R11, R13, R14.
- F5. **Legacy-home migration**
  - **Trigger:** The new runtime opens an existing home.
  - **Actors:** A4, A1.
  - **Steps:** The daemon obtains the home lock, inspects schema and Markdown, plans and applies migrations, verifies node/run invariants, assigns `homeId`, and records the runtime version.
  - **Outcome:** Existing data opens without manual rewrite; an unsupported future schema fails write-capable open with `SCHEMA_TOO_NEW` after a strictly read-only diagnostic preflight and returns no opened home.
  - **Covered by:** R6, R8, R17.

### Acceptance Examples

- AE1. **Concurrent clients create and update**
  - **Covers:** R2, R4, R9.
  - **Given:** DSH and a reference harness connect to the same workspace home.
  - **When:** They submit concurrent creates and revision-checked updates.
  - **Then:** IDs are unique within the qualified home, one conflicting update is rejected with structured conflict details, and Markdown contains no lost write.
- AE2. **Crash between file and SQLite stages**
  - **Covers:** R7, R11.
  - **Given:** A mutation has a durable operation record.
  - **When:** The process is terminated before and after every injectable write boundary and success acknowledgement.
  - **Then:** Unacknowledged prepared work follows its deterministic rollback/roll-forward rule; acknowledged work always rolls forward to the same result, revision, and event cursor; neither path leaves a partial file or duplicate event.
- AE3. **Pause races with claim**
  - **Covers:** R10.
  - **Given:** A run is running with one pending item.
  - **When:** One client pauses while another claims.
  - **Then:** Transaction order yields either a valid claim before pause or a paused rejection; no claim succeeds after the pause commit.
- AE4. **Stale executor reports a newer attempt**
  - **Covers:** R10, R12.
  - **Given:** Attempt one expired and the item was retried as attempt two.
  - **When:** The attempt-one executor submits `done`.
  - **Then:** The runtime rejects the stale lease and leaves attempt two unchanged.
- AE5. **Event consumer reconnects**
  - **Covers:** R11, R13, R14.
  - **Given:** An adapter stored cursor N and disconnected.
  - **When:** Several changes commit and the adapter reconnects from N.
  - **Then:** It receives every later event once in cursor order or is told to perform a bounded snapshot resync when retention expired.
- AE6. **Existing v3 home migrates**
  - **Covers:** R6, R8, R17.
  - **Given:** A current OMT home contains Markdown nodes and DB-only runs.
  - **When:** The daemon opens it for the first time.
  - **Then:** Nodes, bodies, paths, IDs, run items, attempts, and statuses remain equivalent after migration.
- AE7. **Future schema is protected**
  - **Covers:** R5, R8.
  - **Given:** A home declares a schema newer than the runtime supports.
  - **When:** A client tries to open it.
  - **Then:** The runtime returns `SCHEMA_TOO_NEW`, performs no writes, and exposes diagnostic version details.
- AE8. **DSH compatibility**
  - **Covers:** R13, R17.
  - **Given:** The migrated DSH package is installed under the existing package name.
  - **When:** A user exercises tools, skills, tree/detail UI, run UI, references, and notifications.
  - **Then:** Existing public names and behavior remain available while runtime calls go through the client.
- AE9. **External harness portability**
  - **Covers:** R14, R15, R22.
  - **Given:** A harness knows only MCP or the published runtime client.
  - **When:** It discovers tools, creates a ticket, starts a run, claims, reports, handles an attention event, and reconnects.
  - **Then:** The flow succeeds without loading Cordis or any `@deepseek-ai/*` package and receives the same qualified context and recovery options as DSH.
- AE10. **Cross-home and path authorization**
  - **Covers:** R12, R20.
  - **Given:** Two principals own different workspace homes.
  - **When:** One principal supplies the other's `homeId`, a traversal path, a symlink/junction escape, or swaps a validated path before open.
  - **Then:** Query, mutation, event replay, registration, and reindex all fail closed without touching the target.
- AE11. **Daemon control plane and resource limits**
  - **Covers:** R12, R21.
  - **Given:** Clients race daemon startup or submit excessive connections, payloads, command IDs, event backlog, and reindex work.
  - **When:** Bootstrap, authentication, rotation, and quotas are exercised.
  - **Then:** Exactly one daemon generation wins, stale descriptors and credentials fail, healthy clients remain serviceable within documented fairness, and low-disk mode preserves recovery.
- AE12. **Human administration stays human-authorized**
  - **Covers:** R10, R12, R22.
  - **Given:** A normal DSH or MCP agent principal and a separately authorized human administrator.
  - **When:** Each tries to confirm, reject, or override another executor's item.
  - **Then:** The ordinary principal is denied; the administrator action is explicit, audited, qualified to one home/item/attempt, and produces a replayable event without exposing the lease token.

### Success Criteria

- Two independent client processes can use one home for 10,000 mixed commands without duplicate committed command IDs, lost accepted updates, cross-attempt reports, or unrecovered operations.
- Crash-injection tests pass at every journal, file replace, DB finalize, and outbox boundary.
- The existing DSH test suite remains green after moving behavior behind the adapter, and a DSH browser smoke test covers the packaged client bundle.
- A packed non-DSH client and MCP adapter install into an empty project without `.dsh-checkout` links or DSH runtime dependencies.

### Scope Boundaries

#### Included

- Local runtime daemon, multi-home catalog, versioned contracts, SDK, durable events, leases, migrations, MCP adapter, DSH adapter migration, reference harness, packaging, and release gates.
- OS-local IPC usage by multiple harnesses under one operating-system user; MCP may expose stdio or an explicitly enabled local Streamable HTTP listener.

#### Deferred to Follow-Up Work

- True parallel run execution for `RunConfig.concurrency > 1`; the runtime must advertise the capability accurately and reject unsupported values instead of treating the current placeholder as implemented.
- Remote deployment, multi-user tenancy, network ACLs beyond local authenticated transport, and hosted synchronization.
- Isolation from a malicious process already running as the same OS user with filesystem/debug access; the OS account is the hard local security boundary, while scoped principals prevent remote-web access, confused-deputy mistakes, and accidental cross-home use.
- Generic Web UI, mobile UI, and replacing the DSH-specific slot UI.
- Alternate storage engines beyond the port contract and the existing SQLite-plus-Markdown implementation.

#### Outside this product identity

- Turning Markdown into a disposable export. User-authored Markdown remains a durable, Git-friendly part of OMT.
- Making MCP protocol details the domain model. MCP remains one transport adapter.

### Dependencies

- Node 22 or a later explicitly supported runtime for the initial `node:sqlite` adapter.
- MCP TypeScript SDK compatible with the selected protocol release. DSH already supports stdio and Streamable HTTP MCP clients in `packages/mcp/mcp-client/src/index.ts` in the DSH checkout.
- Existing DSH Cordis, tools, skills, connection, web-server, agent, and client-slot services for the DSH adapter only.

### Sources

- Repository evidence: `src/index.ts`, `src/host/core.ts`, `src/host/store.ts`, `src/host/files.ts`, `src/host/pool.ts`, `src/host/tools.ts`, `src/host/rpc.ts`, `src/host/changes.ts`, and `src/host/*-hook.ts`.
- DSH integration evidence: `packages/mcp/mcp-client/src/index.ts`, `packages/core/tools/src/index.ts`, `packages/core/agent/src/index.ts`, `packages/core/session/src/index.ts`, and Cordis Service implementations in the DSH checkout.
- Protocol references: [JSON-RPC 2.0](https://www.jsonrpc.org/specification), [MCP 2025-11-25 transports](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports), and [MCP security best practices](https://modelcontextprotocol.io/docs/2025-11-25/tutorials/security/security_best_practices.md).

---

## Planning Contract

### Key Technical Decisions

- KTD1. **Canonical single-writer daemon.** `(session-settled: user-approved — chosen over a pure embedded SDK: multiple harness processes need one serialization and recovery boundary.)` The daemon owns all writes and per-home locks. The SDK is a client contract, not permission for every harness to open SQLite directly. Governs R1, R2, R7, R9.
- KTD2. **Transport-neutral runtime protocol with MCP at the edge.** `(session-settled: user-approved — chosen over MCP as the only protocol: DSH UI, durable event replay, leases, and administration need a richer stable contract.)` Define a `ClientTransport` seam with authentication, cancellation, backpressure, replay, and reconnect requirements. The v1 local transport uses JSON-RPC over an OS-local stream endpoint: Unix domain socket on POSIX and named pipe on Windows. MCP stdio and Streamable HTTP remain independent adapter transports and never define the runtime protocol. Governs R3, R11, R13, R14.
- KTD3. **DSH remains a compatibility adapter; no generic UI in v1.** `(session-settled: user-approved — chosen over building a generic Web UI in the first release: runtime correctness and compatibility are the critical path.)` Keep native DSH tool registration so names remain `omt_*`; using DSH's generic MCP client would rename them under `mcp__<server>__*` and break current skills. Publish a DSH-only `ctx.omt` Cordis service backed by `@omt/client`, and make DSH tools, RPC, and hooks consume that service. Governs R13, R17, R18.
- KTD4. **Qualified resource identity.** Assign each home a durable `homeId` and make public references `{ homeId, nodeId }` or `{ homeId, runId }`. Preserve human-readable per-home IDs and current Markdown parent references. New create commands require an explicitly authorized home; interactive scope choice stays in the adapter and never enters the runtime. The DSH adapter alone resolves legacy bare IDs through workspace-first routing. Governs R4, R17.
- KTD5. **Recoverable dual-store saga while preserving Markdown authority.** Keep user bodies and hand edits authoritative in Markdown, but serialize runtime mutations through an operation journal. The phases are `prepared → files_applied → db_committed → acknowledged`; only the transaction that commits domain rows, the idempotent result, and the complete outbox effect set is the success linearization point. File plans include before/after hashes, recovery copies, directory fsync, and deterministic multi-file moves. A command acknowledged after `db_committed` may only roll forward; earlier unacknowledged work may roll back or roll forward by journal rule. Reindex runs as an exclusive import command. This replaces the current direct writes in `src/host/files.ts` and mixed ordering in `src/host/core.ts`. Governs R6, R7, R8, R19.
- KTD6. **Leases replace DSH session liveness in the core.** Claim creates a fenced lease containing server-bound principal, delegated actor, attempt, expiry, and secret token. Heartbeat, release, expiry, report, retry, and administrative confirmation become runtime operations. Administrative override is a distinct audited human capability, never an ordinary harness permission. Lease secrets are never emitted on shared events. DSH `agent/status` and `agent/disposed` map to these operations; other harnesses implement their own mapping. Governs R10, R12, R13.
- KTD7. **Durable outbox and cursor-based event stream.** Commit domain events with state changes, retain a bounded per-home log, and let clients resume from a cursor. Host-specific actions such as DSH `followup` and `inject` subscribe outside the runtime. This replaces process-local versioning in `src/host/changes.ts`. Governs R11, R13, R14.
- KTD8. **Contract-first workspace packages.** Use a pnpm workspace with `@omt/contracts`, `@omt/runtime`, `@omt/storage-sqlite-markdown`, `@omt/client`, `@omt/runtime-server`, `@omt/mcp-server`, and the existing `oh-my-ticket` package as the DSH adapter. Generate TypeScript declarations and JSON Schema from the same contract source. Governs R3, R5, R16.
- KTD9. **Three independent compatibility versions.** Track npm SemVer, transport protocol major/capabilities, and monotonic DB schema migrations separately. A package release does not imply a protocol or schema major. Governs R3, R8, R16, R17.
- KTD10. **Authenticated control plane and least privilege.** A per-user bootstrap lock elects one daemon; an atomically published descriptor carries instance generation and endpoint but not a reusable administrator secret. Clients register or derive short-lived credentials scoped to actor namespace, homes, and operations. Every home operation rechecks canonical-path containment and principal authorization. Credentials rotate on daemon generation and never appear in argv, URLs, shared events, logs, or crash reports. Governs R12, R20, R21.
- KTD11. **Two-stage legacy takeover.** An unmodified v0.3 writer cannot honor a future daemon lock. First release a lock-aware compatibility adapter and require all known OMT processes to upgrade or stop; then take a consistent home backup, verify quiescence, write the owner/schema fence, and start daemon ownership. The migration fails closed if an old writer is detected, but documentation explicitly states that an unknown unupgraded binary cannot be technically fenced before takeover. Governs R2, R8, R17.

### High-Level Technical Design

Runtime topology:

```mermaid
flowchart TB
  DSH[DSH host browser and other DSH plugins] --> DA[oh-my-ticket adapter and ctx.omt service]
  EXT[Other harness] --> MCP[MCP adapter process]
  SDK[Programmatic consumer] --> CL[@omt/client]
  DA --> CL
  MCP --> CL
  CL --> IPC[JSON-RPC over Unix socket or named pipe]
  IPC --> SRV[Single-writer runtime daemon]
  SRV --> APP[@omt/runtime application services]
  APP --> SM[@omt/storage-sqlite-markdown]
  SM --> DB[(SQLite metadata runs journal events)]
  SM --> MD[Markdown content tree]
```

Compile-time dependency direction:

```mermaid
flowchart LR
  C[@omt/contracts] --> R[@omt/runtime]
  C --> CL[@omt/client]
  R --> S[@omt/runtime-server]
  R --> ST[@omt/storage-sqlite-markdown]
  C --> S
  CL --> M[@omt/mcp-server]
  C --> M
  CL --> D[oh-my-ticket DSH adapter]
  C --> D
  ST --> S
```

The daemon exposes only the versioned runtime protocol through an authenticated OS-local stream endpoint. `@omt/mcp-server` is an independent adapter process that offers MCP stdio or an explicitly configured MCP Streamable HTTP listener and calls the daemon through `@omt/client`; the runtime server never imports MCP. A protected per-user control plane uses a bootstrap lock, atomic descriptor generation, daemon instance generation, stale cleanup, and short-lived scoped credentials.

Each opened home has one in-process command queue and one inter-process lock. Queries may run concurrently only against committed state. Mutations pass through the queue, validate `commandId` and `expectedRevision`, and acknowledge only after journal recovery conditions are durable and domain rows, the idempotent result, and the complete outbox effect set share one committed linearization point.

### Runtime Contract Shape

The contract package owns these families without exposing SQLite rows or Cordis values:

- Handshake: supported protocol versions, server version, capabilities, actor principal, event retention, and supported run concurrency.
- Home: resolve/register/list home and return stable `homeId`, kind, display path, schema version, and revision.
- Node commands and queries: create, update, move, list, get, tree, search, and reindex.
- Run commands and queries: create, add, list, get, start, pause, resume, cancel, claim, heartbeat, report, confirm/reject, retry, remove, and release.
- Events: committed node/run/lease/attention changes with `eventId`, cursor, home revision, actor, command ID, timestamp, kind, and versioned payload.
- Problems: stable code, summary, details, retryability, protocol version, command ID, and correlation ID.

Public writes require `commandId`. Writes against mutable resources accept `expectedRevision`. The runtime returns the committed revision and emitted event cursor. Retrying a completed command returns the stored result; reusing a command ID with different input is a conflict.

### Persistence and Migration Design

The first new schema introduces:

- A durable `homeId` and home metadata record.
- `schema_migrations` with version, name, checksum, applied timestamp, and runtime version.
- `operations` for command idempotency, journal phase, input hash, stable result envelope, before/after file hashes, recovery copies, and deterministic multi-file plans.
- Per-resource revisions or a home revision plus resource revision where update conflicts need precision.
- `events` as the durable outbox and replay source; one report effect set includes the target run item, ticket state and Markdown, cross-run observations, terminal run derivation, stop-on-failure, and all resulting events.
- Lease and attempt fields sufficient to fence claims and reports.
- Run-member identity snapshots plus quarantine metadata so missing nodes cannot silently wedge, disappear from, or rebind within run history.

Opening an existing v1-v3 home begins with a strictly read-only future-schema preflight before any write-capable PRAGMA, WAL checkpoint, DDL, or migration. After quiescence and exclusive ownership, migration creates a consistent home recovery bundle using SQLite's backup/checkpoint mechanism plus a checksum manifest of Markdown and runtime metadata. It then migrates, verifies nodes/edges/runs/items/counters, assigns `homeId`, and records checksums. The documented rollback restores the whole bundle, not only `omt.db`.

Reindex first emits a zero-write dry-run plan. A missing member in an active run is quarantined with its identity and prior state preserved; the item becomes `interrupted`, cannot be claimed/reported/retried until the node is restored or an administrator resolves it, and the run is re-derived. A terminal member remains historical through its snapshot. Reindex never silently deletes a member or binds it to a different node that reused the display ID.

### System-Wide Impact

- **Data lifecycle:** Runs are DB-only today, so deleting `omt.db` cannot remain described as a full recovery path. Documentation and backup behavior must distinguish Markdown-recoverable node content from DB-only run history.
- **Identity:** Bare IDs are no longer globally meaningful across homes. UI display can remain unchanged, but APIs and events carry qualified references.
- **Security:** Current loopback RPC trusts a supplied `sessionId`. The runtime must distinguish connection principal, delegated actor, and administrator; scope each to homes and operations; canonicalize and contain paths; rotate credentials; and redact paths, bodies, tokens, leases, and high-cardinality metadata from default diagnostics.
- **Concurrency:** `BEGIN IMMEDIATE` in claim is insufficient because run-status validation occurs before the transaction. Claim, pause, cancel, retry, report derivation, and terminal derivation must use transactional predicates and one complete effect set.
- **Notifications:** `idle-hook.ts`, `disposed-hook.ts`, `notify-hook.ts`, and `messages.ts` become DSH policy/delivery code over runtime events and leases rather than core dependencies. Portable attention events state `requiredAction`, qualified resource, reason, deadline, and safe recovery options so any harness can resume without parsing DSH prose.
- **Agent parity:** UI, DSH tools, MCP, and SDK are checked against one action/context matrix. Human administrative confirmation and scope selection remain explicit capabilities; ordinary agent tools cannot silently acquire them.
- **Resource governance:** Per-principal and per-home limits cover payload size, connections, queues, opened homes, search/reindex time, event backlog, command/event retention, log cardinality, and disk reserve. Low-disk mode rejects new mutations while keeping recovery and read diagnostics available.
- **Schemas:** Tool schemas in `src/host/tools.ts`, zod schemas in `src/host/rpc.ts`, and client DTOs must converge on generated/shared contracts.
- **Packaging:** DSH-specific `window.__ModuleLoader__`, linked `@deepseek-ai/*` packages, and `cordis.patch.yml` remain confined to `oh-my-ticket`.

### Risks and Mitigations

- **Big-bang regression:** Use a strangler migration. Keep a compatibility facade with the current `OmtCore` method names until contract tests prove each command through the new runtime.
- **Dual-store recovery complexity:** Build crash-injection support before moving production mutations. Do not claim daemon readiness until every write boundary has a recovery test.
- **Manual Markdown edits during daemon operation:** Reindex acquires the home lock and imports a snapshot. Detect checksum drift before a conflicting write and return a structured reindex-required conflict rather than overwriting edits.
- **Mixed-version writers:** A lock-aware compatibility release precedes daemon takeover. Migration requires quiescence, a consistent backup, and an owner/schema fence; the plan does not pretend an unknown unmodified v0.3 binary can be made to honor a new lock.
- **Protocol lock-in:** Keep domain commands independent of local IPC and MCP. Version envelopes and capabilities, not socket framing or MCP tool names, are the compatibility owner.
- **Control-plane compromise:** Descriptor publication, stale cleanup, credential rotation, actor delegation, home authorization, and secret delivery receive negative tests before multi-client rollout. MCP bridges use least-privilege credentials delivered outside argv and scrub inherited sensitive environment.
- **DSH behavior drift:** Preserve the current tool/RPC renderers and UI-facing DTO projection in the adapter until browser and tool snapshots pass.
- **MCP feature mismatch:** Maintain an explicit parity matrix. MCP exposes every portable agent action, while replay subscriptions and human administration use advertised capabilities rather than silent omissions.
- **Unsupported concurrency promise:** Advertise `runConcurrency: 1` and reject larger values until true parallel dispatch is implemented and tested.

### Sequencing

1. Freeze observable behavior with contract fixtures and introduce shared schemas.
2. Extract pure domain and application services behind the current `OmtCore` facade.
3. Make storage recoverable, migratable, revisioned, and lock-aware; wire the lock into the compatibility facade and release the takeover bridge before any daemon owns real homes.
4. Add leases, durable events, and host-neutral actionable attention events.
5. Add the daemon and client SDK, then prove multi-process correctness and quiescent legacy takeover.
6. Add the independent MCP adapter and a non-DSH reference harness.
7. Move DSH tools/RPC/hooks/UI onto `ctx.omt` and remove direct storage access.
8. Package, migrate, document, canary, and release.

---

## Implementation Units

### U1. Establish the workspace and versioned contract package

- **Goal:** Create the stable protocol vocabulary before moving behavior.
- **Requirements:** R3, R4, R5, R16, R22.
- **Files:** `pnpm-workspace.yaml`, `package.json`, `tsconfig.base.json`, `packages/contracts/package.json`, `packages/contracts/src/index.ts`, `packages/contracts/src/schema.ts`, `packages/contracts/src/actions.ts`, `packages/contracts/src/node.ts`, `packages/contracts/src/run.ts`, `packages/contracts/src/events.ts`, `packages/contracts/src/problems.ts`, `packages/contracts/tests/contracts.spec.ts`, `packages/contracts/tests/parity.spec.ts`, `packages/contracts/tests/compatibility.spec.ts`.
- **Approach:** Convert the repository into a real workspace while keeping the existing package operational. Define public camelCase DTOs, qualified refs, protocol envelopes, capability handshake, stable problem codes, primitive action metadata, agent/adapter/human capability classification, and schema generation. Map current snake_case storage rows only inside adapters.
- **Patterns:** Follow the explicit contract/implementation split used by DSH services such as `packages/jobs/jobs/src/index.ts` in the DSH checkout, but do not introduce a Cordis dependency.
- **Test Scenarios:**
  - Every command, query, event, and problem envelope validates against the exported schema.
  - Additive unknown response fields are ignored by an older v1 client fixture.
  - Unsupported protocol majors fail negotiation with `UNSUPPORTED_PROTOCOL`.
  - Qualified node and run refs reject missing or malformed `homeId`.
  - Every primary action has one primitive contract entry and an explicit agent, adapter, or human-administration classification.
  - Stable error tests assert code/details and never parse message text.
- **Verification:** The contracts package builds declarations, packs cleanly, and installs in an empty TypeScript project without DSH packages.
- **Dependencies:** None.

### U2. Extract pure domain rules and an application runtime facade

- **Goal:** Separate node/run behavior from SQLite, Markdown, clocks, events, and host sessions.
- **Requirements:** R1, R3, R10.
- **Files:** `packages/runtime/package.json`, `packages/runtime/src/index.ts`, `packages/runtime/src/domain/nodes.ts`, `packages/runtime/src/domain/runs.ts`, `packages/runtime/src/domain/transitions.ts`, `packages/runtime/src/application/runtime.ts`, `packages/runtime/src/application/commands.ts`, `packages/runtime/src/application/queries.ts`, `packages/runtime/src/ports.ts`, `packages/runtime/tests/nodes.spec.ts`, `packages/runtime/tests/runs.spec.ts`, `packages/runtime/tests/application.spec.ts`, `src/host/core.ts`.
- **Approach:** Move hierarchy validation, archive rules, run/item transitions, terminal derivation, trust policy, and report semantics into pure decision functions. Introduce ports for unit of work, documents, clock, IDs, leases, and events. Keep `src/host/core.ts` as a temporary compatibility facade that delegates to the runtime.
- **Patterns:** Preserve scenarios from `tests/core.spec.ts`, `tests/archived.spec.ts`, and `tests/run-core.spec.ts` as characterization coverage before changing names or structure.
- **Test Scenarios:**
  - Every current legal and illegal node hierarchy transition remains equivalent.
  - Every run/item transition, stop-on-failure rule, retry, replay, and terminal derivation remains equivalent.
  - Domain tests run with in-memory ports and no filesystem, SQLite, Cordis, or timers.
  - `concurrency > 1` is rejected unless the capability is enabled.
  - Report authorization distinguishes executor lease, stale lease, and administrative confirmation.
- **Verification:** Runtime tests pass with a dependency graph that contains no `@deepseek-ai/*`, React, or `node:sqlite` import.
- **Dependencies:** U1.

### U3. Build recoverable SQLite-plus-Markdown storage and migrations

- **Goal:** Make acknowledged mutations crash-recoverable and legacy homes safely migratable.
- **Requirements:** R6, R7, R8, R9, R17, R19.
- **Files:** `packages/storage-sqlite-markdown/package.json`, `packages/storage-sqlite-markdown/src/index.ts`, `packages/storage-sqlite-markdown/src/store.ts`, `packages/storage-sqlite-markdown/src/documents.ts`, `packages/storage-sqlite-markdown/src/journal.ts`, `packages/storage-sqlite-markdown/src/recovery.ts`, `packages/storage-sqlite-markdown/src/backup.ts`, `packages/storage-sqlite-markdown/src/home-lock.ts`, `packages/storage-sqlite-markdown/src/migrations/index.ts`, `packages/storage-sqlite-markdown/src/migrations/0004-runtime-foundation.ts`, `packages/storage-sqlite-markdown/src/invariants.ts`, `packages/storage-sqlite-markdown/tests/migrations.spec.ts`, `packages/storage-sqlite-markdown/tests/recovery.spec.ts`, `packages/storage-sqlite-markdown/tests/backup-restore.spec.ts`, `packages/storage-sqlite-markdown/tests/reindex.spec.ts`, `packages/storage-sqlite-markdown/tests/concurrency.spec.ts`.
- **Approach:** Port `src/host/store.ts`, `src/host/files.ts`, and `src/host/markdown.ts` behind the runtime ports. Add a read-only future-schema preflight, migration ledger, consistent home-bundle backup, home lock, phased command journal, atomic file replacement with directory fsync, before/after hashes, content-addressed recovery copies, revisions, minimal durable outbox, busy timeout, and bounded storage-busy handling. Define complete effect sets for report and multi-file move. Make reindex an exclusive journaled import with deterministic dry-run and quarantine semantics. Wire the new owner lock and quiescence check into the temporary `src/host/core.ts` facade so a compatibility release can precede daemon takeover.
- **Patterns:** Keep Markdown serialization and managed-child behavior from `src/host/markdown.ts`; replace direct `writeFile` in `src/host/files.ts` with atomic replacement.
- **Test Scenarios:**
  - Fresh latest schema and v1, v2, and v3 fixtures migrate to the latest schema without semantic loss.
  - A future schema fails without modifying DB or files.
  - Re-running a completed migration is idempotent; checksum mismatch fails closed.
  - Process termination before and after each create/update/move/report linearization and acknowledgement boundary recovers according to R7; acknowledged results never roll back.
  - Report crash injection covers item transition, ticket Markdown/state, cross-run observation, run derivation, and outbox as one effect set.
  - Move crash injection covers temp fsync, rename, old/new parent directory fsync, subtree paths, frontmatter, and both managed-child blocks; unexpected manual drift fails closed.
  - A backup fixture with uncheckpointed WAL, DB-only active/history runs, and hand-edited Markdown restores as one verified home bundle.
  - Two storage instances cannot own the same home simultaneously.
  - Manual file drift causes a structured conflict before overwrite; exclusive reindex imports it.
  - Reindex dry-run and quarantine handle missing active and terminal members without delete, rebind, or wedge.
  - Future-schema open leaves DB, WAL/SHM, directories, and Markdown byte-for-byte unchanged.
- **Verification:** Storage invariant checks pass after migration, crash recovery, and reindex; existing Markdown fixtures remain byte-stable outside managed metadata.
- **Dependencies:** U1, U2.

### U4. Add qualified homes, revisions, executor leases, and durable events

- **Goal:** Replace cwd/session assumptions with portable runtime concepts.
- **Requirements:** R4, R9, R10, R11, R12, R22.
- **Files:** `packages/runtime/src/application/homes.ts`, `packages/runtime/src/application/leases.ts`, `packages/runtime/src/application/events.ts`, `packages/runtime/src/application/attention.ts`, `packages/storage-sqlite-markdown/src/home-catalog.ts`, `packages/storage-sqlite-markdown/src/leases.ts`, `packages/storage-sqlite-markdown/src/outbox.ts`, `packages/runtime/tests/leases.spec.ts`, `packages/runtime/tests/attention.spec.ts`, `packages/runtime/tests/revisions.spec.ts`, `packages/storage-sqlite-markdown/tests/outbox.spec.ts`, `packages/storage-sqlite-markdown/tests/lease-concurrency.spec.ts`.
- **Approach:** Assign durable home identities, make all public operations qualified, fence writes with revisions, and implement atomic lease transitions. Move run-state validation into the same transaction as claim. Bind leases to a server-derived principal, delegated actor namespace, attempt, and secret token. Separate ordinary, adapter-only, and human-administrative capabilities. Emit committed domain and actionable attention events with replay cursors and retention metadata.
- **Patterns:** Replace `OmtCorePool`'s cross-home counter synchronization with per-home IDs plus qualified refs; replace `janitorSweep` session probes with lease expiry/release.
- **Test Scenarios:**
  - The same display ID in two homes remains unambiguous through qualified refs.
  - Pause/cancel racing claim has a linearizable result.
  - Two claimers never receive the same item.
  - A foreign actor, agent-less caller, expired lease, or previous attempt cannot report an item.
  - A normal harness principal cannot invoke human confirmation or administrative override; authorized use is audited to home/item/attempt.
  - Heartbeat extends only the matching live lease, and reconnect does not silently transfer lease identity.
  - Event replay resumes from a cursor and signals snapshot resync after retention expiry.
  - Attention events carry required action, reason, qualified resource, deadline, and recovery options without a lease secret.
  - Reusing a command ID with identical input returns the prior result; different input conflicts.
- **Verification:** Multi-worker stress tests preserve transition invariants and event order under concurrent commands.
- **Dependencies:** U3.

### U5. Implement the runtime daemon and client SDK

- **Goal:** Provide the canonical local multi-client process boundary.
- **Requirements:** R2, R3, R9, R11, R12, R16, R20, R21.
- **Files:** `packages/client/package.json`, `packages/client/src/index.ts`, `packages/client/src/client.ts`, `packages/client/src/transport.ts`, `packages/client/src/ipc-transport.ts`, `packages/client/src/events.ts`, `packages/runtime-server/package.json`, `packages/runtime-server/src/index.ts`, `packages/runtime-server/src/server.ts`, `packages/runtime-server/src/jsonrpc.ts`, `packages/runtime-server/src/ipc-server.ts`, `packages/runtime-server/src/auth.ts`, `packages/runtime-server/src/discovery.ts`, `packages/runtime-server/src/bootstrap-lock.ts`, `packages/runtime-server/src/home-manager.ts`, `packages/runtime-server/src/resource-limits.ts`, `packages/runtime-server/tests/server.spec.ts`, `packages/runtime-server/tests/auth.spec.ts`, `packages/runtime-server/tests/path-security.spec.ts`, `packages/runtime-server/tests/resource-limits.spec.ts`, `packages/runtime-server/tests/multiprocess.spec.ts`, `packages/client/tests/client.spec.ts`.
- **Approach:** Serve the shared contract through JSON-RPC on a Unix domain socket or Windows named pipe with lifecycle, health, handshake, command/query, cancellation, backpressure, and event notifications. Use a per-user bootstrap lock, atomically published generation descriptor, stale cleanup, server-derived scoped credentials, and explicit rotation/revocation. Authorize every operation by principal/home/capability after canonical-path containment checks. Let one daemon manage several homes, one serialized command queue per home, bounded resources, and recovery before readiness.
- **Patterns:** Use reconnect and generation-swap ideas from DSH's `packages/mcp/mcp-client/src/index.ts`, while keeping OMT transport contracts independent of Cordis.
- **Test Scenarios:**
  - Concurrent bootstrap yields one daemon generation; stale descriptor, partial descriptor, PID reuse, and old credentials fail closed.
  - Two authorized client processes discover one daemon and share a home.
  - Invalid, expired, cross-actor, cross-capability, or cross-home credentials cannot query, mutate, replay events, reindex, or administer.
  - Traversal, encoded path variants, symlink/junction/hardlink escape, and validation-to-open path replacement fail without touching the target.
  - Server restart recovers incomplete operations before readiness and rotates instance credentials according to policy.
  - Client retry after lost acknowledgement returns the idempotent prior result.
  - Event stream reconnect resumes from the stored cursor with bounded backlog and slow-consumer handling.
  - Payload, connection, queue, open-home, search, reindex, operation/event retention, and low-disk limits degrade safely and fairly.
  - Shutdown drains or rejects new writes without corrupting acknowledged commands; diagnostics redact secrets, bodies, and unsafe paths.
- **Verification:** A 10,000-command multi-process stress test and forced-restart suite meet the Product Contract success criteria.
- **Dependencies:** U4.

### U6. Add MCP and a non-DSH reference harness

- **Goal:** Prove that another harness can use OMT without DSH dependencies.
- **Requirements:** R14, R15, R16, R20, R21, R22.
- **Files:** `packages/mcp-server/package.json`, `packages/mcp-server/src/index.ts`, `packages/mcp-server/src/tools.ts`, `packages/mcp-server/src/results.ts`, `packages/mcp-server/src/stdio.ts`, `packages/mcp-server/src/http.ts`, `packages/mcp-server/src/credentials.ts`, `packages/mcp-server/tests/tools.spec.ts`, `packages/mcp-server/tests/parity.spec.ts`, `packages/mcp-server/tests/transports.spec.ts`, `packages/mcp-server/tests/security.spec.ts`, `examples/reference-harness/package.json`, `examples/reference-harness/src/index.ts`, `examples/reference-harness/tests/e2e.spec.ts`.
- **Approach:** Build `@omt/mcp-server` as an independent adapter that depends only on contracts and `@omt/client`, never runtime storage or server internals. Generate MCP tool inputs and structured outputs from shared contracts and the action-parity matrix. Offer stdio by default and an explicitly enabled MCP Streamable HTTP listener with MCP Host/Origin/auth controls. Give the adapter a least-privilege credential scoped to selected homes and portable actions; deliver secrets outside argv and scrub inherited sensitive environment. Human administration remains outside ordinary MCP tools.
- **Patterns:** Match MCP's standard tool discovery/call behavior and DSH's existing stdio/Streamable HTTP client support. Follow the official MCP transport contract rather than the current private DSH RPC envelope.
- **Test Scenarios:**
  - The parity matrix proves every portable agent action has an MCP tool with the same qualified context, revision, and recovery guidance; human-only operations are absent.
  - Node CRUD, run claim/report, and attention handling succeed over stdio and optional MCP Streamable HTTP.
  - Structured runtime problems map to MCP errors without losing code/details.
  - Tool cancellation propagates to the client without committing an unacknowledged command.
  - Bridge credentials cannot register homes, replay administrative events, confirm another executor, or access an unscoped home; argv, stderr, errors, and child environment reveal no runtime secret.
  - MCP HTTP rejects unexpected Host, Origin, CORS, DNS-rebinding forms, unauthenticated health/handshake/event access, and oversized or slow clients.
  - The reference harness reconnects and uses the same daemon concurrently with a second client.
- **Verification:** The packed MCP package runs in an empty project and the reference harness completes AE9.
- **Dependencies:** U5.

### U7. Convert the existing package into the DSH adapter

- **Goal:** Preserve the current DSH experience while removing direct core/storage ownership.
- **Requirements:** R12, R13, R17, R22.
- **Files:** `packages/dsh-plugin/package.json`, `packages/dsh-plugin/cordis.patch.yml`, `packages/dsh-plugin/src/index.ts`, `packages/dsh-plugin/src/host/service.ts`, `packages/dsh-plugin/src/host/tools.ts`, `packages/dsh-plugin/src/host/rpc.ts`, `packages/dsh-plugin/src/host/events.ts`, `packages/dsh-plugin/src/host/idle-hook.ts`, `packages/dsh-plugin/src/host/disposed-hook.ts`, `packages/dsh-plugin/src/host/notify-hook.ts`, `packages/dsh-plugin/src/host/skill.ts`, `packages/dsh-plugin/src/client/**`, `packages/dsh-plugin/tests/service.spec.ts`, `packages/dsh-plugin/tests/tools.spec.ts`, `packages/dsh-plugin/tests/rpc.spec.ts`, `packages/dsh-plugin/tests/hooks.spec.ts`, `packages/dsh-plugin/tests/client-smoke.spec.ts`.
- **Approach:** Move the current package under the workspace while preserving npm name `oh-my-ticket`, `cordis.patch.yml`, `./client`, and DSH browser bundle shape. Publish a DSH-only `ctx.omt` Cordis service that owns client readiness, reconnect, scoped delegation, and disposal; tools, RPC, hooks, and other DSH plugins consume this service rather than direct storage. Register native `omt_*` tools from the parity matrix. Keep `/omt` and `/omt/events` as compatibility projections. Translate DSH agent/session lifecycle into server-authorized actor namespaces, heartbeats, releases, actionable attention events, and followup/inject delivery. Keep recent UI state in the adapter.
- **Patterns:** Preserve renderer text and UI DTOs from `src/host/tools.ts` and `src/host/rpc.ts`; preserve current slot/controller/client structure until compatibility tests pass.
- **Test Scenarios:**
  - `ctx.omt` activates only after client readiness, reconnects without duplicating subscriptions, and disposes all registrations; other DSH plugins cannot access storage directly.
  - Every existing tool name, parameter behavior, render output, and embedded skill remains available and matches the parity matrix.
  - Existing RPC endpoints return compatible UI projections and structured internal errors.
  - UI execute maps to claim/lease and cannot forge another DSH actor or administrator capability.
  - Idle/disposed notifications map actionable attention and lease events into followup/inject without double delivery or prose parsing.
  - SSE refresh resumes from runtime event cursors and survives daemon restart.
  - The packaged `lib/client.js` loads through the DSH module loader and current UI tests pass.
- **Verification:** All migrated existing tests pass, plus a real DSH install/browser smoke covers tools, tree/details, runs, references, and notifications.
- **Dependencies:** U5. U6 may proceed in parallel.

### U8. Finalize compatibility, release automation, and operator documentation

- **Goal:** Make the runtime supportable as a public multi-package product.
- **Requirements:** R8, R16, R17, R18, R19, R20, R21, R22.
- **Files:** `package.json`, `pnpm-workspace.yaml`, `.changeset/config.json`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `scripts/pack-smoke.mjs`, `scripts/migration-smoke.mjs`, `scripts/security-smoke.mjs`, `README.md`, `docs/runtime/architecture.md`, `docs/runtime/protocol.md`, `docs/runtime/action-parity.md`, `docs/runtime/migrations.md`, `docs/runtime/harness-adapter-guide.md`, `docs/runtime/operations.md`, `docs/runtime/security.md`, `docs/runtime/compatibility.md`.
- **Approach:** Add workspace-wide build/type/test scripts, package-content checks, clean-install tests, migration/backup/restore fixtures, security negatives, resource-limit tests, OS/Node matrix, prereleases, provenance, and compatibility tables. Document daemon bootstrap/discovery, scoped credentials, locks, consistent backups, crash recovery, MCP setup, two-stage legacy takeover, action parity, and whole-home rollback. Release canaries before stable 1.0 and require DSH plus reference-harness evidence.
- **Patterns:** Keep the existing DSH setup-link flow only inside the DSH package. Contracts/runtime/storage/client/MCP build and test without it.
- **Test Scenarios:**
  - Every packed package has correct exports, declarations, dependencies, license, and no local `link:` or `.dsh-checkout` leakage.
  - A lock-aware compatibility release is installed first; a fixture proves quiescent takeover, owner fencing, daemon migration, and preserved DSH behavior.
  - An attempted takeover with a known active legacy writer fails; documentation states the limit for unknown unmodified binaries.
  - Whole-home rollback restores an uncheckpointed-WAL fixture plus Markdown manifest and explicitly refuses unsafe downgrade after schema advance.
  - CI runs migration, backup/restore, crash, multi-process, security, resource-limit, parity, MCP, DSH adapter, and browser smoke gates on supported platforms.
  - Prerelease packages install in empty DSH and non-DSH consumers.
- **Verification:** Release dry-run produces a coherent package set and compatibility report; stable release is blocked unless AE1-AE12 pass.
- **Dependencies:** U1-U7.

---

## Verification Contract

| Gate | Command or suite | Applies to | Done signal |
|---|---|---|---|
| Workspace type safety | `pnpm -r typecheck` | U1-U8 | All public packages and adapters typecheck with declarations enabled. |
| Unit and contract tests | `pnpm -r test` | U1-U8 | Domain, contracts, storage, server, MCP, DSH, and client suites pass. |
| Build | `pnpm -r build` | U1-U8 | Every publishable package produces only declared artifacts. |
| Contract compatibility | `pnpm test:contracts` | U1, U5-U8 | Protocol v1 fixtures, capability negotiation, and stable problem codes pass. |
| Migration matrix | `pnpm test:migrations` | U3, U8 | v1-v3 fixtures migrate; future schema is byte-stable; whole-home backup/restore and legacy takeover pass. |
| Crash recovery | `pnpm test:recovery` | U3-U5 | Every injected interruption point converges; acknowledged commands only roll forward and preserve one effect/event set. |
| Multi-process concurrency | `pnpm test:concurrency` | U4-U5 | Stress target passes with no duplicate command, lost write, lease violation, event gap, or split report effect. |
| Security and limits | `pnpm test:security` | U5-U8 | Principal/home/capability, path escape, credential lifecycle, MCP transport, redaction, and resource-limit negatives pass. |
| Agent parity | `pnpm test:parity` | U4, U6-U8 | DSH, MCP, UI, and SDK match the action/context matrix; human administration stays isolated. |
| MCP integration | `pnpm test:mcp` | U6 | Independent stdio and optional MCP Streamable HTTP adapters complete the reference flow through `@omt/client`. |
| DSH compatibility | `pnpm test:dsh` | U7-U8 | `ctx.omt`, existing tools/hooks, and packaged browser smoke pass in a supported DSH checkout. |
| Package smoke | `pnpm test:pack` | U1, U5-U8 | Tarballs install in empty DSH and non-DSH projects with valid exports and types. |

The implementation must retain current characterization scenarios while relocating them. Deleting a legacy test is acceptable only after an equivalent contract/domain/adapter test proves the same behavior and the mapping is visible in the change.

---

## Definition of Done

- R1-R22 are implemented and traced to passing U1-U8 verification.
- The daemon is the only production writer after a documented lock-aware compatibility release, quiescent takeover, consistent backup, and owner/schema fence; the limitation of unknown unmodified legacy binaries is explicit.
- Existing v1-v3 homes migrate and restore without node, Markdown, path, ID, run, attempt, state, or WAL-committed data loss.
- Create, update, move, report, and reindex survive every injected crash boundary; acknowledged operations only roll forward with one stable result and event set.
- Claim/report use fenced leases and close the current cross-session reporting and pause/claim races; only an audited human capability can override another executor.
- Events are durable, replayable, qualified by home, actionable across harnesses, and consumed by both DSH and a reference harness.
- DSH keeps `omt_*` names, skills, UI, references, workspace behavior, and notifications and exposes the client-backed `ctx.omt` service.
- An external harness completes the reference flow through MCP or `@omt/client` without DSH dependencies and satisfies the action/context parity matrix.
- Public packages ship declarations, versioned schemas, clean exports, compatibility, security, parity, migration, and whole-home recovery docs plus pack-smoke evidence.
- Unsupported future schemas, unsupported protocol majors, unsupported run concurrency, unauthorized principals/homes/capabilities, path escapes, stale leases, and exhausted quotas fail safely with stable structured problems.
- Monitoring and logs include daemon generation, home ID, command/correlation ID, migration phase, recovery action, lease transition, and event cursor without leaking credentials, lease tokens, user body content, or unsafe absolute paths.
- All abandoned compatibility shims, dead direct-storage paths, experimental packages, and unused schemas are removed before stable release; only the documented temporary facade remains during prerelease.
