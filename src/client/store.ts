/**
 * Client state types shared by the controller and components. All state lives
 * in the apply closure's snapshot stores (hooks compartment route — the
 * registrant-private reactive fact channel); no module-level handles.
 */

/** One tree node as delivered by the /omt tree endpoint. */
export interface OmtTreeNode {
  readonly id: string
  readonly type: 'epic' | 'story' | 'substory' | 'ticket' | 'subticket'
  readonly title: string
  readonly status: 'open' | 'in_progress' | 'done' | 'blocked' | 'skipped'
  /** Archive is a separate dimension from the lifecycle status. */
  readonly archived: boolean
  readonly priority: number
  readonly path: string
  readonly created_at: string
  readonly updated_at: string
  readonly children: readonly OmtTreeNode[]
}

export interface NodeSummary {
  readonly id: string
  readonly type: OmtTreeNode['type']
  readonly title: string
  readonly status: OmtTreeNode['status']
  readonly archived: boolean
  readonly priority: number
}

export interface DocData {
  /** OMT home of the owning core (absolute path copy). */
  readonly home?: string
  /** Present while a session is executing this ticket. */
  readonly running?: { readonly sessionId: string; readonly sessionLabel: string; readonly since: string }
  readonly node: OmtTreeNode
  readonly parent?: NodeSummary
  readonly children: readonly NodeSummary[]
  readonly body: string
}

export type TreeState =
  | { readonly status: 'idle' }
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly forest: readonly OmtTreeNode[] }
  | { readonly status: 'error'; readonly message: string }

export type DocState =
  | { readonly status: 'idle' }
  | { readonly status: 'loading'; readonly id: string }
  | { readonly status: 'ready'; readonly data: DocData }
  | { readonly status: 'error'; readonly id: string; readonly message: string }

/** The active ticket fact shown in the composer dock strip. */
export interface ActiveInfo {
  readonly id: string
  readonly title: string
  readonly status: OmtTreeNode['status']
  readonly priority: number
}
