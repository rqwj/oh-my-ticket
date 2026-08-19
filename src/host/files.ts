/**
 * File-system layer: node directory layout, slugging, and node file I/O.
 * Layout: `$OMT_HOME/tickets/<ID>-<slug>/<type>.md`, children nested inside
 * their parent's directory. The `path` column in SQLite mirrors the markdown
 * file location relative to the OMT home.
 */
import { mkdir, readdir, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { dirname, join, relative } from 'node:path'
import { OmtError, type NodeType } from './types.ts'
import { parseNodeFile, type ParsedNodeFile } from './markdown.ts'

export const TICKETS_DIR = 'tickets'

/** Filesystem-safe slug; keeps CJK, strips path-hostile characters. */
export function slugify(title: string): string {
  const slug = title
    .trim()
    .replace(/\s+/g, '-')
    .replace(/[/\\:*?"<>|#]/g, '')
    .replace(/-{2,}/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 40)
  return slug === '' ? 'untitled' : slug
}

export class OmtFiles {
  /** @param home - absolute OMT home directory. */
  constructor(readonly home: string) {}

  get ticketsRoot(): string {
    return join(this.home, TICKETS_DIR)
  }

  /** Directory name for a node: `<ID>-<slug>` (stable once created). */
  nodeDirName(id: string, title: string): string {
    return `${id}-${slugify(title)}`
  }

  /** Absolute path of a stored relative node path. */
  abs(relPath: string): string {
    return join(this.home, relPath)
  }

  /**
   * Relative markdown path for a new node under its parent. Root nodes
   * (epics) land directly under `tickets/`.
   */
  pathFor(type: NodeType, id: string, title: string, parentPath?: string): string {
    const dir = this.nodeDirName(id, title)
    const base = parentPath === undefined ? TICKETS_DIR : dirname(parentPath)
    return join(base, dir, `${type}.md`)
  }

  async ensureDirs(): Promise<void> {
    await mkdir(this.ticketsRoot, { recursive: true })
  }

  async writeNode(relPath: string, content: string): Promise<void> {
    const abs = this.abs(relPath)
    await mkdir(dirname(abs), { recursive: true })
    await writeFile(abs, content, 'utf8')
  }

  async readNode(relPath: string): Promise<ParsedNodeFile> {
    try {
      return parseNodeFile(await readFile(this.abs(relPath), 'utf8'))
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
        throw new OmtError('IO', `node file missing: ${relPath}`)
      }
      throw error
    }
  }

  /** Move a node's whole directory (subtree included) to a new location. */
  async moveDir(oldRelPath: string, newRelPath: string): Promise<void> {
    const oldDir = dirname(this.abs(oldRelPath))
    const newDir = dirname(this.abs(newRelPath))
    await mkdir(dirname(newDir), { recursive: true })
    await rename(oldDir, newDir)
  }

  async removeDir(relPath: string): Promise<void> {
    await rm(dirname(this.abs(relPath)), { recursive: true, force: true })
  }

  /**
   * List all node markdown files under `tickets/`, as home-relative paths.
   * A node file is any `<type>.md` whose basename matches a known type.
   */
  async listNodeFiles(): Promise<string[]> {
    const found: string[] = []
    const walk = async (dir: string): Promise<void> => {
      let entries
      try {
        entries = await readdir(dir, { withFileTypes: true })
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') return
        throw error
      }
      for (const entry of entries) {
        const full = join(dir, entry.name)
        if (entry.isDirectory()) {
          await walk(full)
        } else if (entry.isFile() && entry.name.endsWith('.md')) {
          found.push(relative(this.home, full))
        }
      }
    }
    await walk(this.ticketsRoot)
    return found.sort()
  }
}
