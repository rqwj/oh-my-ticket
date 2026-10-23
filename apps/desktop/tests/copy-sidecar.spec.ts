// @vitest-environment node
/**
 * P3-ACC-7：sidecar 暂存按字节流复制——目标不得携带源文件的扩展属性
 * （com.apple.provenance 会让 tauri-build 的 clonefile 复制 EPERM）、可执行
 * 位为 0755、不留临时文件，且覆盖已存在（带 xattr）的旧副本。
 * 用自定义 xattr `com.omt.test` 代替 provenance（后者用户进程写不了）。
 */
import { describe, expect, it } from 'vitest'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { copyBytes } from '../scripts/copy-bytes.mjs'

const xattrs = (path: string) => execFileSync('xattr', [path], { encoding: 'utf8' })

describe.skipIf(process.platform !== 'darwin')('copyBytes (P3-ACC-7)', () => {
  it('不携带扩展属性、0755、无临时文件残留，并覆盖旧副本', () => {
    const dir = mkdtempSync(join(tmpdir(), 'omt-copy-'))
    const source = join(dir, 'omt-daemon')
    writeFileSync(source, '#!/bin/sh\necho hi\n')
    execFileSync('xattr', ['-w', 'com.omt.test', 'carried', source])
    const dest = join(dir, 'omt-daemon-aarch64-apple-darwin')
    writeFileSync(dest, 'stale')
    execFileSync('xattr', ['-w', 'com.omt.test', 'stale', dest])

    copyBytes(source, dest)

    expect(readFileSync(dest, 'utf8')).toBe('#!/bin/sh\necho hi\n')
    expect(statSync(dest).mode & 0o777).toBe(0o755)
    expect(xattrs(dest)).not.toContain('com.omt.test')
    expect(readdirSync(dir).filter(name => name.includes('.tmp-'))).toEqual([])
  })
})
