---
title: OMT bound skill runtime gate
status: approved
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: ce-brainstorm
execution: code
date: 2026-08-21
---

# OMT Bound Skill Runtime Gate

## Goal Capsule

Make OMT-bound CE workflows enforceable at tool execution time while preserving planning and trivial-change paths. The change is complete when native and code-mode skill loads are tracked, premature ticket creation and file mutation are denied, one-shot bypass is audited, and the existing Web host runs the rebuilt plugin.

## Summary

Upgrade OMT bound skills from prompt-only guidance to enforceable workflow prerequisites. A model must not silently skip bound split or implementation skills, including when tools are invoked through `run_code`.

## Problem Frame

Current binding only adds instructions to the system prompt. Sessions have demonstrated three failure modes: skipping OMT entirely, loading split skills but implementing without `ce-work`, and invoking a skill inside `run_code` while hiding its full content from the next model step. Prompt wording and prompt-composition tests cannot prevent these behaviors.

## Requirements

1. A successful bound skill invocation through native tools or `run_code` must be observable by the OMT plugin for the current DSH turn (`agent/pre-step.payload.turn`), across all of that turn's model steps.
2. A bound skill invoked inside `run_code` must deliver its complete rendered instructions to the next model step.
3. After any bound skill dispatch, the same model step must not invoke `omt_create`, `edit`, or `write` before that skill's instructions have reached the following model request; this includes nested calls in the same `run_code`.
4. `omt_create` must reject when the bound split-stage skills have not been loaded in the current DSH turn.
5. Code/file mutation through `edit` or `write` must reject unless the session has an active model-owned `in_progress` OMT execution marker and the bound implementation-stage skills have been loaded in the current DSH turn.
6. Requirements and implementation plan artifacts may be written before ticket creation.
7. A trivial single-file change may use one explicit, auditable, one-shot bypass. A second mutation must require normal OMT workflow.
8. Denials must tell the model exactly which skill or OMT transition is missing and how to retry.
9. Existing behavior must remain unchanged when no relevant skills are bound.

## Success Criteria

- Regression tests reproduce and block the two observed violations: native direct mutation after planning and code-mode mutation after a nested bound-skill load in the same `run_code`.
- A code-mode skill load injects full skill content into the following model step.
- A compliant split → ticket → in-progress → implementation-skill → edit/write sequence succeeds.
- One-shot trivial bypass succeeds once and is then exhausted.
- Prompt composition, OMT tools, and existing test suites remain green.
- The rebuilt plugin is loaded by the existing DSH Web GUI and the behavior is verified without starting a replacement server.

## Key Decisions

- Use OMT plugin hooks (`tools/post-execute` plus a monotonic tool guard) instead of modifying DSH core. (session-settled: user-approved — chosen over prompt-only and bash-wide interception: it closes the observed gaps without risky shell-command heuristics.)
- Enforce `omt_create`, `edit`, and `write` in the first release; do not heuristically parse `bash` commands.
- Preserve the documented trivial-change exception through an explicit one-shot bypass rather than silent model classification.

## Scope Boundaries

### Included

- Per-agent, per-DSH-turn skill-load state.
- Native and code-mode skill invocation handling.
- Runtime denial for OMT creation and file mutation.
- One-shot trivial bypass tool and audit-friendly result.
- Unit/integration coverage and Web-host reload verification.

### Deferred

- Parsing arbitrary shell commands to determine whether `bash` mutates files.
- Persisting gate state across process restarts.
- Automatically judging whether a user request is “substantial” from natural language.

### Outside this change

- Changing CE skill content or methodology.
- Reworking the OMT hierarchy or run execution model.
- Retrofitting old sessions or tickets.

## Dependencies / Assumptions

- DSH continues to route code-mode nested tools through the normal tool pipeline and forwards `additionalContexts` from nested results.
- `agent/pre-step` exposes numeric `turn` and `step`: a `turn` change resets skill/bypass state, while `step` increments within the same turn retain loaded skills and bypass state.
- The active-ticket source remains `RunningRegistry` for model-driven `in_progress` transitions.

## Product Contract Preservation

Product Contract unchanged; technical planning adds enforcement mechanics without changing the confirmed scope.

## Technical Approach

### 1. Pure per-agent gate state

Add `src/host/skill-gate.ts` with a small state machine keyed by session id. The `agent/pre-step` waterfall listener must always `return await next()` after bookkeeping: compare `payload.turn`, reset task state only when that value changes, and retain loaded skills and bypass across `payload.step` increments. When the step increases within the same turn, promote successful bound skills pending from the previous step into visible load credit, then clear the prior-step delivery lock. Each state records:

- current DSH turn and latest step;
- bound skill names already visible to the model in that turn;
- successful bound skill names pending promotion on the next step;
- current-step delivery lock plus triggering `rootCallId` values;
- one-shot trivial bypass allowance.

For implementation subagents, prerequisite lookup walks the full `session.header.parentSession` chain through the live agent registry: loaded implementation skills and active `RunningRegistry` ownership may be inherited from any ancestor session, while bypass allowances remain local to the caller.

Keep policy decisions pure and separately testable: required split skills, required implementation skills, requirements/implementation-plan artifact allowance, missing prerequisites, and one-shot bypass consumption. Export and reuse the stage-classification helper from `src/host/prompt.ts` so prompt guidance and runtime policy cannot maintain parallel skill-name sets. The pre-ticket artifact allowlist first POSIX-normalizes `edit`/`write` `file_path` values to collapse `.`/`..`, then accepts exactly `.md`/`.html` files whose normalized ancestor directory is named `plans`; this covers both requirements-only and implementation-ready unified plans while excluding arbitrary workspace Markdown and traversal escapes.

### 2. Observe skill calls and ferry code-mode content

Register `tools/pre-execute` for the DSH tool named `skill`: when `arguments.name` is currently bound, arm a session-wide current-step delivery lock and record `rootCallId` before the skill body runs, then delegate with `next()`. This covers native skill batches, sibling roots, and nested same-root calls; a failed skill still blocks mutation for the remainder of that model step but earns no load credit.

Register `tools/post-execute` and always `await next()` before returning the downstream decision:

- ignore failed or unbound skill calls for load credit;
- record a successful bound skill as pending visibility, not loaded; `agent/pre-step` promotes it only when the following model request begins;
- when `exec.parent` exists, extend `src/host/messages.ts` to build an identified plugin message with `form: instructions`, copy the original skill tool's complete rendered `result.content` (not a downstream replacement or spill preview), and prepend it to downstream `additionalContexts` without discarding any downstream decision or context.

Native skill calls continue using their normal tool result and are not duplicated; like nested calls, they receive load credit only at the next `agent/pre-step`.

### 3. Enforce prerequisites with a monotonic guard

Register `ctx.tools.guard` for the relevant calls. Apply checks in this order:

1. For `omt_create`, `edit`, or `write`, first deny while the caller session has any bound-skill delivery lock armed in the current step; no artifact or bypass exception can override this lock. Include the triggering `rootCallId` values in diagnostics.
2. For `omt_create`, require all bound split-stage skills loaded in the current DSH turn.
3. For `edit` / `write`, allow requirements or implementation-plan artifacts only when the POSIX-normalized path ends in `.md`/`.html` and has an ancestor directory named `plans`; otherwise consume a one-shot trivial bypass if armed; otherwise require all bound implementation-stage skills loaded in the current DSH turn and an active model-owned execution marker for the caller session or its `session.header.parentSession` lineage in `RunningRegistry`.

A current-step delivery-lock denial is distinct from a missing-prerequisite denial: it names the pending bound skill(s), does not claim they are unloaded, and tells the model to end the current `run_code`/tool batch and retry on the next model step. Other denials list missing bound skills and missing OMT transitions (active model-owned `RunningRegistry` / `in_progress` marker), plus the exact next action. When neither split-stage nor implementation-stage skills are bound, the guard preserves current behavior.

### 4. Add explicit one-shot bypass

Register `omt_bypass` with a required reason. It arms exactly one subsequent `edit` or `write` attempt in the same DSH turn and returns an auditable result. It does not bypass `omt_create` prerequisites or a current-step pending-delivery lock, and it expires when `agent/pre-step.payload.turn` changes.

Update the always-on prompt to explain that trivial work must use this explicit bypass and that `run_code` skill loading must end the current run before mutation.

## Files to Change

- `src/host/skill-gate.ts` — state machine, hook registration, guard, and `omt_bypass` tool.
- `src/host/messages.ts` — identified plugin instruction message carrying existing rendered content blocks.
- `src/index.ts` — wire the gate to live prompt settings and `RunningRegistry`.
- `src/host/prompt.ts` — document hard-gate sequencing and bypass behavior.
- `tests/skill-gate.spec.ts` — pure policy and hook-level regression tests.
- `tests/prompt.spec.ts` — prompt contract updates.
- `tests/tools.spec.ts` only if the new tool is registered through the existing OMT tool family; otherwise keep its registration assertions isolated.

## Test-First Sequence

1. Add failing tests for split-skill prerequisites on `omt_create`.
2. Add failing tests for active-ticket and implementation-skill prerequisites on `edit`/`write`.
3. Add failing delivery regressions: native and nested bound skill calls arm same-step mutation denial; nested content is attached to `additionalContexts`; successful calls unlock only on the following pre-step.
4. Add failing tests for one-shot bypass, DSH-turn reset, and same-turn step retention.
5. Implement the smallest state machine and registration hooks to pass.
6. Update prompt tests.
7. Run targeted tests, full `pnpm test`, `pnpm run typecheck`, and `pnpm run build`.

## Verification

- Unit evidence: targeted gate and prompt tests.
- Repository evidence: full tests, typecheck, build.
- Host evidence: confirm the existing Web host reloads the rebuilt plugin, inspect the request header/tool catalog, and exercise a controlled session or direct runtime fixture showing denial then compliant success.
- No replacement Web server will be started.

## Risks and Mitigations

- **False positives for trivial work:** explicit one-shot bypass; requirements and implementation-plan artifacts under `plans/` are allowlisted.
- **Stale skill state:** reset on the next DSH turn and plugin reload; step increments and injected contexts within one turn retain skill state.
- **Code-mode context duplication:** attach additional context only for nested calls (`exec.parent`).
- **Bypass abuse:** one mutation only, visible tool call, required reason, and no carry-over.
- **Shell escape hatch:** documented as deferred; no fragile command parser in this change.
- **Local linked dependencies:** the baseline was restored to locked React versions and bundled DSH package symlinks; preserve those links when refreshing `node_modules`. Baseline verification is 33 files / 361 tests passing and build passing. Baseline `tsc --noEmit` has six unrelated errors in `TicketPanel.tsx`, `prompt-settings.ts`, `index.ts`, and `run-components.spec.tsx`; this change must introduce no additional type errors.

## Implementation Units

### U1 — Gate state and policy

- **Goal:** Implement deterministic per-session/per-DSH-turn prerequisite decisions.
- **Files:** `src/host/skill-gate.ts`, `src/host/prompt.ts` (shared stage-classification export), `tests/skill-gate.spec.ts`.
- **Depends on:** none.
- **Scenarios:** empty bindings preserve behavior; missing split/implementation skills deny; `payload.turn` changes reset state while `payload.step` increments retain it; requirements and implementation plans under normalized `plans/` paths remain writable; traversal and arbitrary Markdown do not; bypass is consumed once.

### U2 — Skill observation and code-mode context ferry

- **Goal:** Record successful bound skill calls and attach nested skill content to the next model request.
- **Files:** `src/host/skill-gate.ts`, `src/host/messages.ts`, `tests/skill-gate.spec.ts`.
- **Depends on:** U1.
- **Scenarios:** all pre/post/pre-step waterfall listeners delegate with `next()`; native and nested pre-execute arm a session-step lock before the skill body; successful skills stay pending until the following pre-step then become loaded; native results are not duplicated; nested success attaches the full identified plugin instruction context while preserving downstream contexts; same-step `omt_create`/`edit`/`write` denies before delivery across roots; failed and unbound skills do not unlock the gate.

### U3 — Runtime guard and trivial bypass

- **Goal:** Enforce prerequisites and expose one auditable one-shot bypass.
- **Files:** `src/host/skill-gate.ts`, `src/index.ts`, `tests/skill-gate.spec.ts`.
- **Depends on:** U1, U2.
- **Scenarios:** compliant workflow passes; direct mutation fails; active ticket without implementation skills fails; child and grandchild agents inherit ancestor sessions' loaded implementation skills and active RunningRegistry marks; bypass remains caller-local and allows one mutation attempt only.

### U4 — Prompt and host integration

- **Goal:** Keep model instructions aligned with enforced behavior and ship the rebuilt host plugin.
- **Files:** `src/host/prompt.ts`, `tests/prompt.spec.ts`, generated `lib/*`.
- **Depends on:** U3.
- **Scenarios:** prompt distinguishes current-step delivery locks from unloaded prerequisites, tells the model to end the current `run_code`/batch and retry next step, and explains bypass; full suite/typecheck/build pass; existing GUI host loads new catalog/guard.

## Verification Contract

| Gate | Evidence |
|---|---|
| Policy | `pnpm vitest run tests/skill-gate.spec.ts tests/prompt.spec.ts` |
| Regression | Tests reproduce native direct mutation and same-`run_code` mutation after skill load |
| Repository | `pnpm test`, `pnpm run build`; run `pnpm run typecheck` and prove no errors beyond the six captured baseline failures |
| Host | Existing DSH Web process reload/restart as required; request header contains updated guidance; controlled mutation is denied until prerequisites are met |

## Definition of Done

- All U1–U4 scenarios pass.
- Bound code-mode skill content reaches the following model step.
- `omt_create`, `edit`, and `write` enforce configured prerequisites.
- One-shot bypass is visible, reasoned, and expires after one mutation attempt or a DSH turn change.
- No DSH core files are modified.
- Existing GUI at `http://127.0.0.1:3080` uses the rebuilt plugin.
