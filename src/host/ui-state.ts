/**
 * Saved tree-filter state (STORY-0023): the ticket tree's viewing filters
 * persist per OMT home so a page reload (or reopening the panel) restores
 * them. The file lives at `<home>/ui-filters.json` — inside the workspace's
 * `.omt` when one exists, else the global home. Personal preference data:
 * keep it out of git alongside `omt.db`.
 * @module oh-my-ticket/ui-state
 */
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { z } from 'zod'
import { NODE_TYPES, STATUSES } from './types.ts'

export const SORT_ORDERS = ['none', 'priority-desc', 'priority-asc'] as const
export type SortOrder = (typeof SORT_ORDERS)[number]

/** The persisted bag — every field defaulted, so partial patches merge. */
export const savedFiltersSchema = z.object({
  query: z.string().max(200).default(''),
  showArchived: z.boolean().default(false),
  types: z.array(z.enum(NODE_TYPES)).max(NODE_TYPES.length).default([]),
  statuses: z.array(z.enum(STATUSES)).max(STATUSES.length).default([]),
  priorities: z.array(z.number().int().min(0).max(3)).max(4).default([]),
  showId: z.boolean().default(false),
  sortOrder: z.enum(SORT_ORDERS).default('none'),
}).strict()

export type SavedFilters = z.infer<typeof savedFiltersSchema>

export const DEFAULT_SAVED_FILTERS: SavedFilters = savedFiltersSchema.parse({})

const FILTERS_FILE = 'ui-filters.json'

/** Parse untrusted file content into a full bag; anything invalid → defaults. */
function coerce(raw: unknown): SavedFilters {
  const parsed = savedFiltersSchema.safeParse(raw)
  return parsed.success ? parsed.data : structuredClone(DEFAULT_SAVED_FILTERS)
}

/**
 * Read the saved filters for a home. A missing or corrupt file is not an
 * error — panel loading must never be blocked by preference state.
 */
export async function readSavedFilters(home: string): Promise<SavedFilters> {
  let raw: string
  try {
    raw = await readFile(join(home, FILTERS_FILE), 'utf8')
  } catch {
    return structuredClone(DEFAULT_SAVED_FILTERS)
  }
  try {
    return coerce(JSON.parse(raw))
  } catch {
    return structuredClone(DEFAULT_SAVED_FILTERS)
  }
}

/**
 * Persist one filter bag (already validated) via temp-file + rename so a
 * crash mid-write cannot leave truncated JSON behind.
 */
export async function writeSavedFilters(home: string, filters: SavedFilters): Promise<void> {
  const target = join(home, FILTERS_FILE)
  const staging = `${target}.tmp`
  await mkdir(dirname(target), { recursive: true })
  await writeFile(staging, `${JSON.stringify(filters, null, 2)}\n`, 'utf8')
  await rename(staging, target)
}
