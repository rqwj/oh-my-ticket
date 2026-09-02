/**
 * Saved tree-filter state (STORY-0023): the ticket tree's viewing filters
 * persist per OMT home so a page reload (or reopening the panel) restores
 * them. Since U7a the BAG is persisted by the omt-daemon (`ui/filters-get|set`
 * meta rows, one per home + adapter key) — this module keeps only the
 * client-side contract: the zod schema, the defaulted empty bag, and the
 * coerce helper that degrades corrupt/absent data to defaults.
 * @module oh-my-ticket/ui-state
 */
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

/** Parse untrusted stored content into a full bag; anything invalid → defaults. */
export function coerceSavedFilters(raw: unknown): SavedFilters {
  const parsed = savedFiltersSchema.safeParse(raw)
  return parsed.success ? parsed.data : structuredClone(DEFAULT_SAVED_FILTERS)
}
