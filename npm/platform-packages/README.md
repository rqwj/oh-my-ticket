# npm platform packages (U13, R21/KD1)

`@oh-my-ticket/<platform>-<arch>` packages are the DSH adapter's INTERNAL
fallback channel for `omt-daemon`: they carry the same binaries as the GitHub
Release archives, laid out for `require.resolve('@oh-my-ticket/<triple>/bin/omt-daemon')`
(see `packages/client-ts/src/daemon-resolve.ts`, KTD7 precedence level 4).
They are NOT a primary product channel — brew tap and install.sh are (KD1).

## Directory ↔ triple map

| directory      | Rust target triple            | os      | cpu    |
| -------------- | ----------------------------- | ------- | ------ |
| darwin-arm64   | aarch64-apple-darwin          | darwin  | arm64  |
| darwin-x64     | x86_64-apple-darwin           | darwin  | x64    |
| linux-arm64    | aarch64-unknown-linux-gnu     | linux   | arm64  |
| linux-x64      | x86_64-unknown-linux-gnu      | linux   | x64    |

## Fill contract (release.yml job `npm-platform-packages`)

Templates in this directory are the source of truth; the workflow must agree
with them. On a `v*` tag push the job:

1. downloads job1's `release-binaries-<tag>` artifact and verifies
   `SHA256SUMS` first (KTD8 — nothing unverified is ever packed);
2. extracts each `omt-<triple>-v<version>.tar.gz`;
3. per template dir: copies `<archive>/{omt-daemon,omt}` into
   `<dir>/bin/` (creating `bin/`), so the layout after filling is exactly:

   ```
   npm/platform-packages/<dir>/
   ├── package.json   (template)
   ├── bin/
   │   ├── omt-daemon
   │   └── omt
   ```

4. stamps THIS tag's product version into each package:
   `(cd <dir> && npm version $version --allow-same-version --no-git-tag-version)`
   — the committed version placeholder is `0.0.0-semantically-released`
   (R22: platform packages track the workspace product version; the npm root
   package keeps its own independent SemVer line);
5. runs `npm pack --dry-run` per package (`files: ["bin"]` → tarball contains
   `bin/omt-daemon`, `bin/omt`, plus auto-included package.json/README);
6. publishes with `npm publish --access public` only when `NPM_TOKEN` is set;
   otherwise an explicit notice records the clean skip.

## Template invariants

- `name` MUST equal `@oh-my-ticket/<directory name>` (the resolver derives it
  from `process.platform`/`process.arch`).
- `os`/`cpu` arrays MUST match the triple table above — they gate installation
  on foreign platforms at npm-install time.
- `bin` MUST map both `omt-daemon` and `omt` to `bin/<name>`; the resolver only
  consumes `bin/omt-daemon`, but both binaries ship so `npx omt doctor` works.
- NO `exports` field: deep-file resolution
  (`@oh-my-ticket/<dir>/bin/omt-daemon`) must stay reachable.
- Unknown directory names fail the workflow loudly (extend its
  `dir_to_triple` mapping together with this table).

The root package will list all four under `optionalDependencies` pinned to
the workspace product version (`0.2.0` today): installing `oh-my-ticket` from
npm pulls the matching triple automatically, while absence/404 stays
non-fatal — resolution then falls back to PATH/prefixes and finally errors
with product channel guidance.

**Sequencing (frozen-lockfile constraint)**: the declaration lands in root
`package.json` only AT the first platform publish. Before that, committed
`package.json` omits it — pnpm silently drops unresolvable optional deps
from the lockfile, which breaks `--frozen-lockfile` specifier parity in
CI. The root-publish flow therefore must: (1) run the platform-package
publish job first, (2) stamp the `optionalDependencies` block into
`package.json` (same stamping pass as `npm version`), (3) pack/publish the
root. Source checkouts are unaffected — they resolve the daemon from
`target/` builds or product-channel installs either way.
