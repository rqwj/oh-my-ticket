# YAML byte-stability corpus — conventions

This corpus pins the **exact full-file bytes** of OMT node Markdown files so
the TypeScript serializer (`src/host/markdown.ts`) and the Rust codec
(`crates/omt-domain/src/markdown.rs`) stay byte-identical for content the
runtime does not change. Every case stores the input surface plus
`expectedBase64`: the exact UTF-8 bytes returned by the REAL TS serializer
(`serializeNodeFile(attrs, replaceChildrenBlock(body, renderChildrenBlock(children, ...)))`).

Regenerate with:

```sh
node scripts/gen-yaml-goldens.mjs        # rewrites cases.json from the fixture list in the script
```

## Pinned dump options

`serializeNodeFile` calls `yaml.dump(attrs, { lineWidth: -1, noRefs: true })`
(js-yaml 4.3.1, DEFAULT_SCHEMA incl. implicit timestamp/merge types). The load-bearing consequences:

1. **Key order** = insertion order of `frontmatterOf` (`src/host/core.ts`):
   `id, type, title, status, [archived], priority, [parent], created_at,
   updated_at`. `archived` appears only when true; `parent` only when set.
   Consumers must rebuild this order, never rely on JSON object key order.
2. **lineWidth -1** disables folding entirely: no STYLE_FOLDED output, long
   lines are never wrapped.
3. **Scalar style choice** mirrors js-yaml's dumper exactly:
   - empty string → `''`;
   - strings matching the YAML-deprecated booleans (`y Y yes Yes YES n N no
     No NO on On ON off Off OFF`) or base-60 shape `/^[-+]?[0-9_]+(?::[0-9_]+)+(?:\.[0-9_]*)?$/`
     (e.g. `12:34`) → force-single-quoted (noCompatMode=false compat);
   - any non-printable code point (per js-yaml `isPrintable`: excludes TAB,
     CR, LF-as-content is handled by styles, DEL, C1 controls except none,
     U+0085, U+00A0, U+2028/9, BOM) → double-quoted with js-yaml escape table;
   - otherwise plain when plain-safe char-wise AND not implicitly resolvable
     as null/bool/int/float/timestamp/merge under the schema resolvers;
   - single-quoted (`''` escaping) when plain-safe fails or ambiguous;
   - multiline values (contain `\n`) → **literal block** `|`, `|-`, `|`
     with chomp per trailing-newline shape and a numeric indentation
     indicator when a line after a newline starts with a space.
4. **Trailing newline rule**: `yaml.dump` returns `dump + "\n"`;
   `serializeNodeFile` does `.trimEnd()` before embedding between the `---`
   fences, then appends `\n---\n\n` and a body with leading `\n+` stripped.
   The final attribute is always `updated_at` (single-line ISO string), so
   the global trimEnd can never eat a meaningful literal-block terminator;
   this invariant is pinned here deliberately.
5. **Slice semantics** (orchestrator decision 3): directory slugs come from
   `title.slice(0, 40)` in TypeScript = **UTF-16 code unit** truncation. A
   cut landing inside a surrogate pair yields a lone surrogate, which Node's
   UTF-8 writer emits as U+FFFD. The Rust side must truncate by UTF-16 units
   with lossy decode (straddled pair → one U+FFFD) — proven two ways: the
   `children-astral-slug-parity` case (`"🦀".repeat(30)` child → slug of
   20 crabs, even-boundary cut) and its `TICKET-0998` sibling
   (`"🦀x".repeat(21)` → the 40-unit cut lands mid-pair, so the slug ends in
   U+FFFD), whose bytes flow into the parent file through the children
   block link.

## Differential harness invocation

`crates/omt-storage/tests/yaml_differential.rs` generates ≥2000 seeded
randomized adversarial inputs (fixed LCG seed committed in the test), writes
them as one JSON array to a scratch file, then invokes the TS leg **once per
test binary run** in a single node process:

```sh
node scripts/gen-yaml-goldens.mjs --stdin < inputs.json > expectations.json
```

stdin: `[{"name":..., "attrs":{...}, "body":..., "children":[...]}]`,
stdout: `[{"name":..., "expectedBase64":...}]`. Total runtime budget: <60 s.
Node must be on PATH (the repo is an npm package; this is a given in CI).

## Known non-goals

- Lone surrogates cannot cross the JSON transport (serde_json and strict
  JSON parsers reject them). Slug-generated dirNames are therefore recorded
  as `dirNameBase64` — the exact UTF-8 bytes Node's writer produced (an
  unpaired surrogate already became U+FFFD inside the file bytes). Astral
  parity is exercised through whole code points everywhere else, which is
  what real titles contain.
- JS `trimEnd()` trims U+FEFF but not U+0085; Rust `trim_end()` is the exact
  opposite. Both sides agree on every reachable input because the trimmed
  region is always the `updated_at` line (rule 4); do not move a
  whitespace-suffixed field last without revisiting this file.
- JS `Number.prototype.toString` vs serde_json shortest-repr can differ for
  exotic float magnitudes; frontmatter numerics are runtime-controlled
  integer priorities, so the differential harness only generates safe
  integers for numeric fields.
