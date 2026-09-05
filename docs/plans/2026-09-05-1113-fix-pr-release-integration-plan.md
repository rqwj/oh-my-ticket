---
title: "fix: Integrate existing product changes into PR 8 at 0.6.4"
date: 2026-09-05
type: fix
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: ce-plan-bootstrap
execution: code
---

## Goal Capsule
Update the existing upstream PR 8 with the user's complete product branch and version 0.6.4 while preserving credential recovery and the primary checkout.

## Product Contract
- R1. Preserve the entire history and product results from feat/omt-always-on-prompt, not only its tip commit, together with fix/credential-renewal.
- R2. Synchronize product version 0.6.4 according to existing release conventions.
- R3. Include the offered .gitignore worktree rule and credential-recovery plan; exclude .dsh local files, caches and secrets. Leave the primary checkout unchanged.
- R4. Update only the existing fork-backed PR. No force-push, rebase, merge-to-main, installation, Web restart, deployment, npm publication or tag.

## Planning Contract
Normal merge preserves both histories and avoids accidentally dropping earlier UI, prompt settings and runtime skill-gate work. Inspect conflicts rather than choosing either side blindly.
`scripts/sync-version.mjs` uses the workspace version in `Cargo.toml` and propagates root/desktop/Tauri versions. `Cargo.lock` and `tests/mocks/fixtures.ts` must match. Platform package templates intentionally retain their release placeholder; optional dependency floors remain stable across patch releases. Inspect the TypeScript client manifest and pnpm lockfile rather than rewriting unrelated dependency versions.
The user directed broad integration rather than a credential-only PR; its larger review surface is intentional. Preserve existing product commits without opportunistic refactors or repairs of unrelated test races.

## Implementation Units
### U1. Verified PR integration
Covers R1-R4. Integrate the existing branch in its existing isolated worktree, copy only offered nonlocal files, and synchronize canonical product version consumers. Files: `Cargo.toml`, `Cargo.lock`, `package.json`, `apps/desktop/package.json`, `apps/desktop/src-tauri/tauri.conf.json`, `tests/mocks/fixtures.ts`, `.gitignore`, and the offered credential plan under `docs/plans/`; merged source and test files retain their existing product behavior.
Dependencies: live PR remains open at the expected head before integration and push. This is a single delivery outcome; integration and verification share the branch and cannot be parallel-authored safely.
Execution note: no new behavior is authored; reuse regression tests as integration evidence and inspect both ancestry and final diff. Runtime/browser verification must not change the installed environment.

## Verification Contract
- Confirm both original credential-fix and entire product-branch tips are ancestors of the result; inspect source overlap and ensure credential recovery stays intact.
- Audit every version consumer and fixture, leaving intentional independent client versions and publishing placeholders unchanged.
- Run the 15 credential-renewal tests, complete TypeScript suite, root/client typechecks, plugin and daemon builds, and runtime Rust tests. Record new totals and investigate any failure; known heartbeat cleanup ENOTEMPTY is not silently counted as success.
- Review affected UI through available tests/builds; report that live 0.6.4 UI cannot be validated without installation/restart rather than testing an older installed build as if it were this branch.
- Verify exact fork head before normal push; re-read live PR SHA/body after update. Take a bounded CI/review snapshot without approving maintainer-gated workflows.

## Definition of Done
PR 8 remains open with the verified 0.6.4 head and accurate body; no newly introduced blocking defect remains. Report commit/merge SHAs, actual tests and UI limits, OMT result, excluded files, unchanged main-checkout residuals, and CI state. If external permissions or concurrent mutation prevents safe completion, preserve state and report the exact blocker.
