#!/usr/bin/env node
/**
 * Version lockstep (KTD1 unification): the root Cargo.toml
 * `[workspace.package]` version is the CANONICAL product version; every
 * surface carries the SAME number. This script propagates it:
 *
 *   node scripts/sync-version.mjs           # re-sync consumers ← Cargo.toml
 *   node scripts/sync-version.mjs 0.6.0     # bump Cargo.toml first, then sync
 *
 * Consumers rewritten:
 *   - package.json                          (npm root = the DSH plugin)
 *   - apps/desktop/package.json             (desktop shell)
 *   - apps/desktop/src-tauri/tauri.conf.json (Tauri bundle / app version —
 *     feeds the「安装 DSH 插件」flow: it installs oh-my-ticket@<app version>)
 *
 * NOT touched: the npm/platform-packages templates keep the
 * `0.0.0-semantically-released` placeholder — release.yml stamps the real
 * version (the same workspace version) at publish time (KTD1 job 3).
 */
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(fileURLToPath(import.meta.url), '..', '..')

// Optional explicit argument: bump the canonical Cargo.toml line first.
const arg = process.argv[2]
const cargoPath = join(root, 'Cargo.toml')
let cargo = readFileSync(cargoPath, 'utf8')
if (arg !== undefined) {
  if (!/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(arg)) {
    console.error(`error: '${arg}' is not a plain semver (major.minor.patch[-prerelease])`)
    process.exit(1)
  }
  cargo = cargo.replace(/^version = ".*"$/m, `version = "${arg}"`)
  writeFileSync(cargoPath, cargo)
}

// Canonical read (after the optional bump).
const canonical = cargo.match(/^\[workspace\.package\][^[]*?^version = "([^"]+)"/m)?.[1]
if (canonical === undefined) {
  console.error('error: cannot read [workspace.package] version from Cargo.toml')
  process.exit(1)
}

/** Rewrite the package's own `"version"` field (first match from file top). */
function syncJson(path) {
  const full = join(root, path)
  const json = readFileSync(full, 'utf8')
  const pattern = /^(\s*"version":\s*)"[^"]*"/m
  if (!pattern.test(json)) {
    console.error(`error: no "version" field found in ${path}`)
    process.exit(1)
  }
  writeFileSync(full, json.replace(pattern, `$1"${canonical}"`))
  console.log(`  ${path} → ${canonical}`)
}

/**
 * The root package references its platform-package optionalDependencies with
 * a caret floor at the canonical MAJOR.MINOR.0 (^0.6.0): the specifier stays
 * STABLE across patch releases so pnpm's frozen-lockfile check never sees a
 * stale entry, while consumers still resolve the newest daemon in the line
 * (protocol compatibility is negotiated at handshake). Rewrite every
 * @oh-my-ticket/* entry in place; no-op when the field is absent.
 */
function syncOptionalDeps() {
  const full = join(root, 'package.json')
  const pkg = JSON.parse(readFileSync(full, 'utf8'))
  const optional = pkg.optionalDependencies ?? {}
  const floor = `^${canonical.split('.').slice(0, 2).join('.')}.0`
  let touched = false
  for (const name of Object.keys(optional)) {
    if (name.startsWith('@oh-my-ticket/') && optional[name] !== floor) {
      optional[name] = floor
      touched = true
    }
  }
  if (touched) {
    writeFileSync(full, JSON.stringify(pkg, null, 2) + '\n')
    console.log(`  package.json optionalDependencies → ${floor}`)
  }
}

console.log(`canonical version: ${canonical} (Cargo.toml [workspace.package])`)
syncJson('package.json')
syncJson('apps/desktop/package.json')
syncJson(join('apps', 'desktop', 'src-tauri', 'tauri.conf.json'))
syncOptionalDeps()
console.log('lockstep OK')
