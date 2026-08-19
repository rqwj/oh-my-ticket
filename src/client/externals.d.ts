/**
 * Ambient declarations for platform modules answered by the browser loader
 * module table at runtime (see tsdown.config.ts CLIENT_EXTERNALS). Declared
 * structurally so this out-of-tree package typechecks without the monorepo
 * type graph; shapes copied from their sources.
 */

declare module '@deepseek-ai/dsh-client-runtime/client' {
  /** Bare observable snapshot (hook-binding identity must stay stable). */
  export interface ObservableSnapshot<T> {
    getSnapshot(): T
    subscribe(fn: () => void): () => void
  }
  /** Snapshot store with immer-draft mutation (runtime contract/store.ts). */
  export interface SnapshotStore<T> extends ObservableSnapshot<T> {
    update(mutator: (draft: T) => void): void
    set(next: T): void
  }
  export function createSnapshotStore<T>(
    init: T,
    opts?: { flush?: 'raf' | 'sync'; persist?: { name: string } },
  ): SnapshotStore<T>
}

declare module '@deepseek-ai/dsh-client-ui-slots' {
  /**
   * Namespace merge table (ui-slots index.ts): dictionary owners declare-merge
   * their namespace key unions in. Declared empty here so this out-of-tree
   * package's own merge (locales.ts) typechecks without the monorepo graph.
   */
  export interface LocaleNamespaceMap {}
}

declare module '@deepseek-ai/dsh-client-ui-primitives' {
  import type { ComponentType } from 'react'
  /** Markdown renderer (ui-primitives markdown/MarkdownText.tsx). */
  export const MarkdownText: ComponentType<{ text: string; streaming?: boolean }>
}
