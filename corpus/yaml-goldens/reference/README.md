# Frozen TypeScript serializer reference (U4b differential/golden truth)

Byte-exact snapshots of the retired direct-storage sources, extracted at
commit `fcc444e~1` — the last state before U7a deleted them. Content is
identical to what generated `corpus/yaml-goldens/cases.json` at `f5c218e`
(U4b): none of the three files changed between golden generation and deletion.

| File | Origin | Role in the reference build |
|---|---|---|
| `types.frozen.ts` | `src/host/types.ts` | frontmatter/DTO types + `OmtError` (value import kept by transpile) |
| `files.frozen.ts` | `src/host/files.ts` | `slugify` (UTF-16 slice semantics) |
| `markdown.frozen.ts` | `src/host/markdown.ts` | `serializeNodeFile`, `renderChildrenBlock`, `replaceChildrenBlock` |

Consumed only by `scripts/gen-yaml-goldens.mjs` (compiled on the fly via
`typescript.transpileModule` into `node_modules/.omt-goldens/*.gen.mjs`):

- default mode regenerates the golden fixtures (`cases.json`);
- `--stdin` mode answers the Rust differential leg
  (`crates/omt-storage/tests/yaml_differential.rs`) with TS expectations.

These are a frozen artifact of the pinned dump conventions. Do not edit them
to make a failing comparison pass; re-baselining is a deliberate migration
commit (plan stop condition: Markdown byte stability).
