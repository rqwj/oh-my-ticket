/**
 * P3-ACC-7：按字节流复制可执行文件。`copyFileSync` 在 macOS 走 clonefile，
 * 会把源文件的扩展属性（cargo 产物带 com.apple.provenance）一并克隆；sidecar
 * 变化触发 tauri-build 重跑时，它的 copy_binaries 再克隆这个带 provenance
 * 的文件会被系统拒绝（EPERM），且 target/debug/omt-daemon 已被它先删掉。
 * 读写字节流得到的新 inode 不带任何 xattr；先写同目录临时名再 rename，
 * 避免半截文件被别的构建步骤拿去用。
 */
import { chmodSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs'

export function copyBytes(source, dest) {
  const tmp = `${dest}.tmp-${process.pid}`
  try {
    writeFileSync(tmp, readFileSync(source), { mode: 0o755 })
    chmodSync(tmp, 0o755) // umask 可能削掉 writeFileSync 的 mode
    renameSync(tmp, dest)
  } catch (error) {
    rmSync(tmp, { force: true })
    throw error
  }
}
