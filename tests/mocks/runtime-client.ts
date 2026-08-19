/**
 * Test double for the '@deepseek-ai/dsh-client-runtime/client' platform
 * module (answered by the loader table in the browser; aliased here so the
 * controller's snapshot stores run under plain node).
 */
export interface ObservableSnapshot<T> {
  getSnapshot(): T
  subscribe(fn: () => void): () => void
}

export interface SnapshotStore<T> extends ObservableSnapshot<T> {
  update(mutator: (draft: T) => void): void
  set(next: T): void
}

export function createSnapshotStore<T>(init: T, _opts?: { flush?: 'raf' | 'sync'; persist?: { name: string } }): SnapshotStore<T> {
  let snapshot = init
  const listeners = new Set<() => void>()
  const notify = (): void => {
    for (const fn of listeners) fn()
  }
  return {
    getSnapshot: () => snapshot,
    subscribe(fn) {
      listeners.add(fn)
      return () => {
        listeners.delete(fn)
      }
    },
    update(mutator) {
      // Structural-share the top level; test scenarios mutate scalars/arrays.
      const draft = (Array.isArray(snapshot) ? [...snapshot] : { ...snapshot }) as T
      mutator(draft)
      snapshot = draft
      notify()
    },
    set(next) {
      snapshot = next
      notify()
    },
  }
}
