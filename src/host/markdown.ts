/**
 * Node Markdown codec: YAML frontmatter (metadata mirror) + free-form body +
 * a plugin-managed children section delimited by HTML comment markers. The
 * managed block is regenerated from the SQLite edges whenever relations
 * change; user edits outside the markers are authoritative content.
 */
import yaml from 'js-yaml'
import { OmtError, type NodeFrontmatter, type OmtNode } from './types.ts'

const FRONTMATTER_PATTERN = /^---\n([\s\S]*?)\n---\n?/

/** Begin/end markers of the managed children block inside the body. */
export const CHILDREN_BEGIN = '<!-- omt:children -->'
export const CHILDREN_END = '<!-- /omt:children -->'

export interface ParsedNodeFile {
  readonly attrs: Partial<NodeFrontmatter>
  readonly body: string
}

/** Split a node file into frontmatter attributes and body text. */
export function parseNodeFile(content: string): ParsedNodeFile {
  const match = FRONTMATTER_PATTERN.exec(content)
  if (match === null) return { attrs: {}, body: content }
  let attrs: Partial<NodeFrontmatter>
  try {
    const parsed: unknown = yaml.load(match[1] ?? '')
    attrs = (parsed ?? {}) as Partial<NodeFrontmatter>
  } catch (error) {
    throw new OmtError('INVALID_INPUT', `invalid frontmatter YAML: ${(error as Error).message}`)
  }
  return { attrs, body: content.slice(match[0].length) }
}

/** Serialize frontmatter attributes + body into a full node file. */
export function serializeNodeFile(attrs: NodeFrontmatter, body: string): string {
  const frontmatter = yaml.dump(attrs, { lineWidth: -1, noRefs: true }).trimEnd()
  return `---\n${frontmatter}\n---\n\n${body.replace(/^\n+/, '')}`
}

/**
 * Render the managed children block: a `## 子节点` list of relative links,
 * one per child, ordered by edge ord. Links are relative to the parent
 * node's own directory, so moving a whole subtree keeps them valid.
 */
export function renderChildrenBlock(children: readonly OmtNode[], childDirName: (child: OmtNode) => string): string {
  const lines = children.map(child =>
    `- [${child.id} ${child.title}](${childDirName(child)}/${child.type}.md) — ${child.status}`)
  const list = lines.length > 0 ? lines.join('\n') : '（暂无子节点）'
  return `${CHILDREN_BEGIN}\n## 子节点\n\n${list}\n${CHILDREN_END}`
}

/**
 * Replace the managed children block inside a body, or append it when the
 * markers are absent (e.g. a hand-written file that never had one).
 */
export function replaceChildrenBlock(body: string, block: string): string {
  const begin = body.indexOf(CHILDREN_BEGIN)
  const end = body.indexOf(CHILDREN_END)
  if (begin >= 0 && end > begin) {
    return body.slice(0, begin) + block + body.slice(end + CHILDREN_END.length)
  }
  const trimmed = body.replace(/\s+$/, '')
  return trimmed === '' ? `${block}\n` : `${trimmed}\n\n${block}\n`
}

/** Strip the managed children block, returning the user-owned body only. */
export function stripChildrenBlock(body: string): string {
  const begin = body.indexOf(CHILDREN_BEGIN)
  const end = body.indexOf(CHILDREN_END)
  if (begin >= 0 && end > begin) {
    return (body.slice(0, begin) + body.slice(end + CHILDREN_END.length)).replace(/\n{3,}/g, '\n\n').trim()
  }
  return body.trim()
}

/** Default body template per node type (children block appended separately). */
export function defaultBody(type: string): string {
  switch (type) {
    case 'epic':
      return '## 目标\n\n\n\n## 范围\n'
    case 'story':
    case 'substory':
      return '## 描述\n\n\n\n## 验收标准\n'
    default:
      return '## 描述\n\n\n\n## 验收标准\n\n\n\n## 进度记录\n'
  }
}
