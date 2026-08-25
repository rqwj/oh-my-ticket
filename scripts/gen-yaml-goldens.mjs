#!/usr/bin/env node
/**
 * YAML byte-stability goldens (plan U4b).
 *
 * Generates adversarial frontmatter fixtures and records the EXACT full-file
 * bytes produced by the PINNED TypeScript serializer: serializeNodeFile +
 * renderChildrenBlock + replaceChildrenBlock (js-yaml 4 dump with
 * { lineWidth: -1, noRefs: true }).
 *
 * Post-U7a the live modules (`src/host/markdown.ts`, `src/host/files.ts`)
 * were retired with the direct-storage path, so the script compiles the
 * FROZEN baseline copies under `corpus/yaml-goldens/reference/` instead.
 * Those copies are the exact sources that generated `cases.json`
 * (extracted at `fcc444e~1`; content-identical to golden generation at
 * `f5c218e`). They are a frozen artifact of the pinned dump conventions —
 * edit them only to re-baseline the goldens deliberately, never to make a
 * failing comparison pass.
 *
 * Modes:
 *   node scripts/gen-yaml-goldens.mjs            regenerate corpus/yaml-goldens/cases.json
 *   node scripts/gen-yaml-goldens.mjs --stdin    read [{name,attrs,body,children?}] JSON array
 *                                                from stdin, write [{name,expectedBase64}] to stdout
 *
 * The TS sources are loaded through `typescript.transpileModule` (the repo's
 * own devDependency) because Node's strip-only mode rejects the parameter
 * properties in the types source. Transpiled artifacts land under
 * node_modules/.omt-goldens/ so bare-specifier resolution still hits the
 * repo's js-yaml; the original sources are never modified.
 */
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const cacheDir = path.join(repoRoot, 'node_modules', '.omt-goldens')
const corpusDir = path.join(repoRoot, 'corpus', 'yaml-goldens')
const referenceDir = path.join(corpusDir, 'reference')

/** Transpile one frozen reference source to an ESM artifact string. */
function transpile(fileName) {
  const full = path.join(referenceDir, fileName)
  const source = fs.readFileSync(full, 'utf8')
  const out = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
    },
    fileName,
  }).outputText
  // Rewrite sibling imports to the transpiled artifact names.
  return out.replaceAll("from './types.ts'", "from './types.gen.mjs'").replaceAll("from './markdown.ts'", "from './markdown.gen.mjs'")
}

function loadRealSerializer() {
  fs.mkdirSync(cacheDir, { recursive: true })
  const modules = {
    'types.gen.mjs': transpile('types.frozen.ts'),
    'markdown.gen.mjs': transpile('markdown.frozen.ts'),
    'files.gen.mjs': transpile('files.frozen.ts'),
  }
  for (const [name, code] of Object.entries(modules)) {
    fs.writeFileSync(path.join(cacheDir, name), code)
  }
  // Dynamic import AFTER all artifacts exist.
  const markdownUrl = path.join(cacheDir, 'markdown.gen.mjs')
  return import(markdownUrl)
}

// ── fixture corpus ───────────────────────────────────────────────────────

/**
 * One fixture = one full serializeNodeFile call surface:
 * attrs are built with EXACTLY the `frontmatterOf` spread order from
 * src/host/core.ts (`id,type,title,status,[archived],priority,[parent],
 * created_at,updated_at`) — yaml.dump preserves insertion order, so building
 * in any other order would pin bytes the runtime can never produce.
 * Children entries are rendered through the REAL renderChildrenBlock; body
 * goes through the REAL replaceChildrenBlock.
 */
const T0 = '2026-08-24T05:00:00.000Z'

function baseAttrs(title, opts) {
  return {
    id: opts.id !== undefined ? opts.id : 'TICKET-0001',
    type: 'ticket',
    title,
    status: opts.status !== undefined ? opts.status : 'open',
    ...(opts.archived === true ? { archived: true } : {}),
    priority: opts.priority !== undefined ? opts.priority : 0,
    ...(opts.parent !== undefined ? { parent: opts.parent } : {}),
    created_at: T0,
    updated_at: T0,
  }
}

const fixtures = []
function add(name, title, opts = {}) {
  fixtures.push({
    name,
    attrs: baseAttrs(title, opts),
    body: opts.body ?? '',
    children: opts.children ?? [],
  })
}

// CJK long / short.
add('cjk-short', '登录问题')
add('cjk-long', '这是一个非常长的中文标题用于测试超长行为它不应该被折行因为lineWidth是负数'.repeat(3))
add('cjk-mixed-punct', '修复：构建失败（含「引号」与"直引号"）', { status: 'in_progress' })

// Emoji incl. astral families (surrogate pairs in JS, single chars in Rust).
add('emoji-bmp', '✅ done ☃ snow')
add('emoji-astral-crab', '🦀'.repeat(30))
add('emoji-astral-mixed', '🦀x'.repeat(21))
add('emoji-family', '👨‍👩‍👧‍👦 family 🏳️‍🌈 flag')
add('emoji-variation', 'Ｓｉｎｇｌｅ①②③ vs ㍿')

// Quotes both kinds.
add('quote-single', "it's a trap")
add('quote-double', 'say "hello" now')
add('quote-both', `both ' and " kinds`)
add('quote-trailing-single', 'ends with apostrophe\'')
add('quote-lone-double-first', '"leading dquote')

// Colons.
add('colon-time-like', '12:34')
add('colon-hms', '12:34:56')
add('colon-space-inside', 'key: value inside title')
add('colon-tight-inside', 'key:value tight')
add('colon-trailing', 'ends with colon:')
add('colon-leading', ':starts with colon')
add('colon-cjk-fullwidth', '全角：冒号不算指示符')

// Leading special characters.
for (const ch of ['-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', "'", '"', '%', '@', '`', '=']) {
  const label = ch === "'" ? 'apos' : ch === '"' ? 'dquote' : ch === '`' ? 'backtick' : encodeURIComponent(ch).replaceAll('%', '')
  add(`lead-${label}`, `${ch} leading ${JSON.stringify(ch)}`)
}
add('lead-dash-nonindicator', '-notanindicatorbutstillquoted')
add('lead-question-nonws', '?tag:value')
add('lead-equals', '= leading equals')
add('merge-key-string', '<<')
add('multiline-indent-indicator', '\n  indented first line after blank')
add('multiline-newline-and-tab', 'line\nnext\thas-tab')

// Boolean/null-like strings.
for (const s of ['true', 'True', 'TRUE', 'false', 'False', 'FALSE', 'null', 'Null', 'NULL', '~', '', 'y', 'Y', 'yes', 'Yes', 'YES', 'n', 'N', 'no', 'No', 'NO', 'on', 'On', 'ON', 'off', 'Off', 'OFF']) {
  add(`boolish-${s === '' ? 'empty' : s}`, s)
}

// Numeric-like strings.
for (const s of ['007', '1e5', '1E5', '0x1F', '0o17', '0b101', '+5', '-5', '.5', '5.', '5.0', '12_000', '123456789012345678901234567890123456789012345678901234567890', '400digits-' + '9'.repeat(380), '-.inf', '.Inf', '.NaN', '+.inf', 'inf', 'NaN', 'Infinity', '-0']) {
  add(`numeric-${s.replace(/[^a-zA-Z0-9]/g, '_').slice(0, 24)}`, s)
}

// Multiline values.
add('multiline-simple', 'line one\nline two')
add('multiline-trailing-nl', 'ends with newline\n')
add('multiline-two-trailing-nl', 'ends with two\n\n')
add('multiline-only-nl', '\n')
add('multiline-empty-lines', 'a\n\nb\n\n')
add('multiline-leading-space-line', 'a\n b more-indented')
add('multiline-tab-inside', 'a\tb tabbed')
add('multiline-cr-only', 'a\rb carriage-return')
add('multiline-crlf', 'a\r\nb windows')
add('multiline-quote-start', 'starts\n"with quote')
add('multiline-hash-line', 'a\n#hash-looking line')
add('newline-in-middle-cjk', '第一行\n第二行')

// Very long lines (> any default lineWidth; dump uses -1 → no folding).
add('long-line-no-fold', 'x'.repeat(500))
add('long-line-spaces-no-fold', ('word '.repeat(120)).trimEnd())
add('long-line-cjk-no-fold', '长'.repeat(300))

// Unicode normalization edges: NFC vs decomposed must round-trip byte-stable.
add('unicode-nfc', 'café-\u00E9-composed')
add('unicode-nfd', 'caf\u0065\u0301-decomposed')
add('unicode-nbsp', 'before\u00A0after-nbsp')
add('unicode-nel', 'beforeafter-nel')
add('unicode-zwj', 'zero\u200Dwidth-joiner')
add('unicode-del-char', 'beforeafter-del')
add('unicode-bom-inside', 'before﻿after-bom')
add('unicode-line-sep', 'before after-ls')

// Timestamp-valued strings.
for (const [label, s] of [
  ['date-only', '2026-08-19'],
  ['iso-full', '2026-08-19T00:00:00.000Z'],
  ['iso-nosecs-frac', '2026-08-19T00:00:00Z'],
  ['single-digit-md', '2026-8-9T05:00:00+08:00'],
  ['space-separator', '2026-08-19 05:00:00'],
  ['tab-separator', '2026-08-19\t05:00:00'],
  ['lowercase-t', '2026-08-19t05:00:00z'],
  ['no-tz', '2026-08-19T05:00:00'],
  ['frac-only', '2026-08-19T05:00:00.5'],
  ['tz-minute', '2026-08-19T05:00:00-0530'],
]) {
  add(`timestamp-${label}`, s)
}

// Field-level variety beyond title (attrs stay in frontmatterOf order).
add('fields-status-weird', 'normal title', { status: 'in_progress' })
add('fields-archived-true', 'normal title', { archived: true })
add('fields-parent', 'normal title', { parent: 'STORY-0007' })
add('fields-negative-priority', 'normal title', { priority: -3 })
add('fields-positive-priority', 'normal title', { priority: 17 })
add('fields-id-numeric-ish', 'normal title', { id: '007' })

// Body shapes.
add('body-empty', 'body empty', { body: '' })
add('body-whitespace', 'body whitespace', { body: '   \n\n  \n' })
add('body-leading-newlines', 'body lead nl', { body: '\n\n\ntext after newlines' })
add('body-trailing-newlines', 'body trail nl', { body: 'text\n\n\n' })
add('body-multiline', 'body multiline', { body: '# 标题\n\n段落一。\n\n- 列表项\n' })

// Children-block interactions (incl. astral slug parity proof case).
add('children-none-marker-append', 'children append', {
  body: 'user content here',
  children: [{ id: 'TICKET-0002', title: '子任务甲', status: 'open', type: 'ticket', dirName: 'TICKET-0002-子任务甲' }],
})
add('children-replace-existing', 'children replace', {
  body: 'before <!-- omt:children -->\n## 子节点\n\n- [TICKET-OLD old](old/ticket.md) — open\n<!-- /omt:children --> after-part',
  children: [{ id: 'TICKET-0003', title: 'stale replaced', status: 'done', type: 'ticket', dirName: 'TICKET-0003-stale_replaced' }],
})
add('children-empty-list-placeholder', 'children empty', {
  body: '',
  children: [],
})
add('children-astral-slug-parity', 'astral parent', {
  body: 'parent body',
  children: [
    {
      id: 'TICKET-0999',
      title: '🦀'.repeat(30),
      status: 'open',
      type: 'ticket',
      dirName: null, // filled below via REAL TS slugify (UTF-16 slice semantics)
      // Marks that this dirName is slug-GENERATED (`<id>-<slug>`), so the
      // Rust leg must recompute it through its own slugify and prove
      // UTF-16-slice parity (decision 3). Hand-written dirNames on other
      // fixtures are opaque strings the serializer passes through verbatim.
      slugGenerated: true,
    },
    {
      // 40 UTF-16 units of '🦀x'.repeat(21) cut MID-SURROGATE-PAIR: unit 39
      // is the high surrogate of the 14th crab, which Node's UTF-8 writer
      // emits as U+FFFD. This is the actual straddle proof for decision 3.
      id: 'TICKET-0998',
      title: '🦀x'.repeat(21),
      status: 'open',
      type: 'ticket',
      dirName: null,
      slugGenerated: true,
    },
    {
      id: 'TICKET-0997',
      title: '👨‍👩‍👧‍👦 family emoji ZWJ 链',
      status: 'done',
      type: 'ticket',
      dirName: null,
      slugGenerated: true,
    },
  ],
})

async function main() {
  const md = await loadRealSerializer()
  const { slugify } = await import(path.join(cacheDir, 'files.gen.mjs'))

  // Fill every slug-generated child dirName through the REAL TS slugify so
  // both legs compare against the same TypeScript truth.
  for (const fixture of fixtures) {
    for (const child of fixture.children) {
      if (child.dirName === null && child.slugGenerated) {
        child.dirName = `${child.id}-${slugify(child.title)}`
      }
    }
  }

  if (process.argv.includes('--stdin')) {
    // Differential mode: read randomized inputs, answer with TS expectations.
    // `attrsPairs` ([key, value] pairs) pins insertion order across the JSON
    // transport (serde_json maps sort alphabetically); `attrs` objects are
    // accepted as-is for direct callers.
    const input = JSON.parse(fs.readFileSync(0, 'utf8'))
    const results = input.map((item) => {
      const attrs = item.attrsPairs ? Object.fromEntries(item.attrsPairs) : item.attrs
      const block = md.renderChildrenBlock(item.children ?? [], (child) => child.dirName)
      const fullBody = md.replaceChildrenBlock(item.body, block)
      const fileText = md.serializeNodeFile(attrs, fullBody)
      return {
        name: item.name,
        expectedBase64: Buffer.from(fileText, 'utf8').toString('base64'),
      }
    })
    process.stdout.write(JSON.stringify(results))
    return
  }

  const cases = fixtures.map((f) => {
    const block = md.renderChildrenBlock(f.children, (child) => child.dirName)
    const fullBody = md.replaceChildrenBlock(f.body, block)
    const fileText = md.serializeNodeFile(f.attrs, fullBody)
    // Slug-generated dirNames may end in a LONE SURROGATE (decision 3
    // straddle case); a lone surrogate cannot cross JSON (serde_json and
    // strict parsers reject it), so those are recorded as UTF-8 BYTES
    // (base64) — exactly the bytes Node's writer put into the file
    // (unpaired surrogate → U+FFFD).
    const children = f.children.map((child) => {
      if (!child.slugGenerated) return child
      const { dirName, ...rest } = child
      return { ...rest, dirNameBase64: Buffer.from(dirName, 'utf8').toString('base64') }
    })
    return {
      name: f.name,
      attrs: f.attrs,
      body: f.body,
      children,
      expectedBase64: Buffer.from(fileText, 'utf8').toString('base64'),
    }
  })

  fs.mkdirSync(corpusDir, { recursive: true })
  const dest = path.join(corpusDir, 'cases.json')
  fs.writeFileSync(dest, JSON.stringify({ cases }, null, 2) + '\n')
  console.log(`gen-yaml-goldens: ${cases.length} cases -> ${path.relative(repoRoot, dest)}`)
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
