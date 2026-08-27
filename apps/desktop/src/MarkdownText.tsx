/**
 * Minimal markdown renderer for ticket bodies (KD4 fresh surface — no
 * DSH MarkdownText reuse). Supports the subset OMT bodies actually use:
 * headings, bullet/numbered lists, bold/italic/inline code, fenced code
 * blocks, links, paragraphs. Builds React elements directly — no
 * dangerouslySetInnerHTML, no XSS surface under the hardened CSP.
 */
import React from 'react'

function inline(text: string, keyBase: string): React.ReactNode[] {
  const nodes: React.ReactNode[] = []
  // Order matters: code spans first (their content is literal), then
  // links, bold, italic.
  const pattern = /(`[^`]+`)|(\[([^\]]+)\]\(([^)]+)\))|(\*\*([^*]+)\*\*)|(\*([^*]+)\*)/g
  let last = 0
  let match: RegExpExecArray | null
  let idx = 0
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > last) nodes.push(text.slice(last, match.index))
    const key = `${keyBase}-${idx++}`
    if (match[1]) {
      nodes.push(<code key={key} className="md-code">{match[1].slice(1, -1)}</code>)
    } else if (match[2]) {
      nodes.push(<span key={key} className="md-link" title={match[4]}>{match[3]}</span>)
    } else if (match[5]) {
      nodes.push(<strong key={key}>{match[6]}</strong>)
    } else if (match[7]) {
      nodes.push(<em key={key}>{match[8]}</em>)
    }
    last = match.index + match[0].length
  }
  if (last < text.length) nodes.push(text.slice(last))
  return nodes
}

export function MarkdownText({ text }: { text: string }) {
  const blocks: React.ReactNode[] = []
  const lines = text.split('\n')
  let i = 0
  let key = 0
  while (i < lines.length) {
    const line = lines[i]
    // Fenced code block
    if (line.trimStart().startsWith('```')) {
      const body: string[] = []
      i += 1
      while (i < lines.length && !lines[i].trimStart().startsWith('```')) {
        body.push(lines[i])
        i += 1
      }
      i += 1 // closing fence
      blocks.push(<pre key={key++} className="md-fence"><code>{body.join('\n')}</code></pre>)
      continue
    }
    // Heading
    const heading = /^(#{1,4})\s+(.*)$/.exec(line)
    if (heading) {
      const level = heading[1].length
      const content = inline(heading[2], `h${key}`)
      blocks.push(
        level === 1 ? <h1 key={key++}>{content}</h1>
        : level === 2 ? <h2 key={key++}>{content}</h2>
        : level === 3 ? <h3 key={key++}>{content}</h3>
        : <h4 key={key++}>{content}</h4>,
      )
      i += 1
      continue
    }
    // List (consecutive bullet/numbered lines)
    if (/^\s*[-*]\s+/.test(line) || /^\s*\d+\.\s+/.test(line)) {
      const ordered = /^\s*\d+\.\s+/.test(line)
      const items: string[] = []
      while (i < lines.length && (/^\s*[-*]\s+/.test(lines[i]) || /^\s*\d+\.\s+/.test(lines[i]))) {
        items.push(lines[i].replace(/^\s*(?:[-*]|\d+\.)\s+/, ''))
        i += 1
      }
      const children = items.map((item, j) => <li key={j}>{inline(item, `li${key}-${j}`)}</li>)
      blocks.push(ordered ? <ol key={key++}>{children}</ol> : <ul key={key++}>{children}</ul>)
      continue
    }
    // Blank line
    if (line.trim() === '') {
      i += 1
      continue
    }
    // Paragraph
    blocks.push(<p key={key++}>{inline(line, `p${key}`)}</p>)
    i += 1
  }
  return <div className="md-body">{blocks}</div>
}
