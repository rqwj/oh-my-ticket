#!/usr/bin/env node
/**
 * repair-session-log.mjs — repair a DSH session log corrupted by a second
 * instance writing to the same $DSH_HOME (duplicate-seq foreign events).
 *
 * Background: two live dsh processes sharing one sessions home each appended
 * their own event with the same seq (observed: a foreign `session/end-seed`
 * seal written by a second instance that had the session open). The loader's
 * continuity check then refuses the whole log:
 *   "corrupt session log: seq gap in committed region at line N (expected E, got G)"
 *
 * What this script does:
 *   1. backs up the original artifact (always, before any write);
 *   2. decodes every line with DSH's own storage decoder and walks seq
 *      continuity exactly like the loader;
 *   3. at a regression, drops the offending line ONLY when it consists
 *      solely of `session/end-seed` events (a foreign seal — semantically
 *      safe to remove; any other pattern aborts with a report);
 *   4. verifies the repaired log with DSH's own scanLog — no scanLog pass,
 *      no write;
 *   5. recompresses as DSH's own concatenated-frame container (first frame
 *      exactly the header line, then one checksummed frame per record, via
 *      DSH's compressZstdFrame) and verifies the artifact through DSH's real
 *      read path (scanZstdFrames + frame decoder + SessionLogScanner) —
 *      a single zstd-CLI frame is NOT loadable by DSH;
 *   6. replaces the artifact (only with --write).
 *
 * Prerequisites:
 *   - the owning dsh instance MUST be stopped first (the file is append-only
 *     and live); run this while no dsh process is using this sessions home;
 *   - `zstd` CLI on PATH (input decode only);
 *   - a local deepseek-harness checkout for the authoritative decoder
 *     (default /Users/robertq/Tools/dsh/deepseek-harness; override with
 *     DSH_CHECKOUT=/path).
 *
 * Usage:
 *   node scripts/repair-session-log.mjs <session-artifact-dir> [--input <file>] [--write]
 * Example:
 *   node scripts/repair-session-log.mjs \
 *     ~/.dsh/sessions/--Users-robertq-...--/session-a9fece4a-... --write
 */
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const CHECKOUT = process.env.DSH_CHECKOUT ?? '/Users/robertq/Tools/dsh/deepseek-harness'
const PERSIST = `file://${CHECKOUT}/packages/session/session-persistence-jsonl/lib/types`
const { scanLog, SessionLogScanner } = await import(`${PERSIST}/format.js`)
const { compressZstdFrame, createZstdFrameDecoder, scanZstdFrames } = await import(`${PERSIST}/zstd.js`)
const { decodeStorageRecord } = await import(`file://${CHECKOUT}/packages/core/session/lib/index.js`)

const dir = process.argv[2]
const write = process.argv.includes('--write')
const inputFlag = process.argv.indexOf('--input')
const inputPath = inputFlag === -1 ? undefined : process.argv[inputFlag + 1]
if (dir === undefined || (inputFlag !== -1 && inputPath === undefined)) {
  console.error('usage: node scripts/repair-session-log.mjs <session-artifact-dir> [--input <file>] [--write]')
  process.exit(2)
}

const artifact = inputPath ?? join(dir, 'session.jsonl.zstd')
const tmp = mkdtempSync(join(tmpdir(), 'omt-session-repair-'))
const rawPath = join(tmp, 'session.jsonl')
execFileSync('zstd', ['-d', '-f', artifact, '-o', rawPath], { stdio: 'pipe' })
const original = readFileSync(rawPath, 'utf8')
const lines = original.split('\n')

// Sanity: the log must currently be broken, otherwise there is nothing to do.
try {
  scanLog(readFileSync(rawPath))
  console.log('scanLog passes on the current artifact — nothing to repair.')
  process.exit(0)
} catch (error) {
  console.log(`current damage: ${error.message}`)
}

// Walk lines with the authoritative decoder, tracking seq continuity. The
// foreign seal (session/end-seed) itself matches the expected seq when it
// appears — the regression only surfaces on the NEXT line. So on a
// regression we look BACK: if the previously kept line is a foreign seal
// covering the same seq, pop it and re-evaluate the current line.
const dropped = [] // { lineNo, type, seq }
const keptLines = [lines[0]]
const keptEvents = [] // decoded events per kept event line (aligned to keptLines[1..])
let expected = 0
let initialized = false

for (let i = 1; i < lines.length; i++) {
  const text = lines[i]
  if (text === '') continue
  let events
  try {
    events = decodeStorageRecord(JSON.parse(text))
  } catch {
    console.error(`line ${i + 1}: unparsable record — aborting, manual inspection needed`)
    process.exit(1)
  }
  const first = events[0]
  if (first === undefined || typeof first.seq !== 'number') {
    keptLines.push(text)
    keptEvents.push([])
    continue
  }
  if (!initialized) {
    expected = first.seq
    initialized = true
  }
  if (first.seq !== expected) {
    // Regression. The only auto-repairable shape: the previously kept line
    // is a foreign session seal duplicating this line's first seq. Real
    // history (user messages, tool calls) is never dropped automatically.
    const prev = keptEvents.at(-1)
    const prevIsSeal = prev !== undefined && prev.length > 0 && prev.every(event => event.type === 'session/end-seed')
    if (prevIsSeal && prev.at(-1).seq === first.seq) {
      dropped.push({ lineNo: i, type: 'session/end-seed', seq: prev.at(-1).seq })
      keptLines.pop()
      keptEvents.pop()
      expected = first.seq
    } else {
      console.error(`line ${i + 1}: seq regression (expected ${expected}, got ${first.seq}) `
        + `without a droppable foreign seal before it — aborting, manual inspection needed`)
      process.exit(1)
    }
  }
  keptLines.push(text)
  keptEvents.push(events)
  expected = events.at(-1).seq + 1
}

if (dropped.length === 0) {
  console.error('no droppable foreign seal found although scanLog failed — aborting')
  process.exit(1)
}
for (const drop of dropped) {
  console.log(`dropping line ${drop.lineNo}: foreign ${drop.type} (seq ${drop.seq})`)
}

// Recompress in DSH's own container shape: the first frame must decode to
// exactly the header line (assertZstdHeaderFrame enforces this), each later
// frame is an independently checksummed record batch. A single whole-file
// zstd frame (e.g. `zstd -19`) passes scanLog but is REFUSED by the loader.
const frames = []
for (const line of keptLines) {
  frames.push(await compressZstdFrame(Buffer.from(`${line}\n`, 'utf8')))
}
const rebuilt = Buffer.concat(frames)
writeFileSync(join(tmp, 'repaired.jsonl.zstd'), rebuilt, { mode: 0o600 })

// Authoritative verification, level 1: the repaired plaintext must pass
// DSH's own scanner.
const repaired = `${keptLines.join('\n')}\n`
const { events, committedBytes } = scanLog(Buffer.from(repaired, 'utf8'))
console.log(`scanLog OK: ${events.length} events preserved, ${committedBytes} bytes committed`)

// Authoritative verification, level 2: the rebuilt artifact must survive
// DSH's real read path (frame scan + per-frame decode + header assertion +
// incremental record scanner), not just whole-file decompression.
{
  const { frames: located, tornStart } = scanZstdFrames(rebuilt)
  if (tornStart !== undefined) throw new Error('rebuilt artifact has a torn final frame')
  if (located.length === 0) throw new Error('rebuilt artifact has no frames')
  const decoder = createZstdFrameDecoder()
  const decoded = decoder.decode(rebuilt, located)
  const headerFrame = decoded.next()
  if (headerFrame.done) throw new Error('rebuilt artifact yields no header frame')
  const header = headerFrame.value
  if (header.length === 0 || header.indexOf(0x0A) !== header.length - 1) {
    throw new Error('rebuilt artifact: first frame is not exactly one header line')
  }
  const scanner = new SessionLogScanner(header)
  for (const plaintext of decoded) scanner.write(plaintext)
  const complete = scanner.checkpoint()
  if (complete.committedBytes !== complete.inputBytes) {
    throw new Error('rebuilt artifact: complete frame contains a torn JSONL record')
  }
  const prefix = scanner.finish()
  if (prefix.events.length !== events.length) {
    throw new Error(`rebuilt artifact: event count drift (${prefix.events.length} != ${events.length})`)
  }
  console.log(`read-path OK: ${located.length} frames, ${prefix.events.length} events, header "${prefix.meta.id}"`)
}

if (!write) {
  console.log('\ndry run — re-run with --write to apply (a backup is made first).')
  process.exit(0)
}

const target = join(dir, 'session.jsonl.zstd')
if (existsSync(target)) {
  const backup = `${target}.bak-${new Date().toISOString().replace(/[:.]/g, '-')}`
  copyFileSync(target, backup)
  console.log(`backup: ${backup}`)
}
copyFileSync(join(tmp, 'repaired.jsonl.zstd'), target)
console.log(`repaired artifact written: ${target}`)
