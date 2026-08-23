/**
 * Client-side shape of the persisted tree-filter bag (STORY-0023). The host
 * validates the same bag with zod (`ui-state.ts`); this mirror exists so the
 * browser half never imports Node modules. Keep both in sync.
 * @module oh-my-ticket/saved-filters
 */
import type { TreeSortOrder } from './tree-filter.ts'

/** The full viewing-filter state of the ticket panel, as persisted. */
export interface SavedFilters {
  readonly query: string
  readonly showArchived: boolean
  readonly types: readonly string[]
  readonly statuses: readonly string[]
  readonly priorities: readonly number[]
  readonly showId: boolean
  readonly sortOrder: TreeSortOrder
}

/** Reset target and hydration fallback — identical to the panel's initial view. */
export const DEFAULT_SAVED_FILTERS: SavedFilters = {
  query: '',
  showArchived: false,
  types: [],
  statuses: [],
  priorities: [],
  showId: false,
  sortOrder: 'none',
}
