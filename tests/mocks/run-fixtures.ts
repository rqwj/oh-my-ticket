/**
 * Shared client-side run fixtures: the zeroed RunProgress and a RunSummary
 * factory with parameterized title/createdAt (run-ui specs use a fixed
 * English label + timestamp; component specs use a 批次 label + a
 * five-minutes-ago timestamp for relative-time rendering).
 */
import type { RunProgress, RunSummary } from '../../src/client/store.ts'

/** Zeroed progress with per-state overrides. */
export function runProgress(overrides: Partial<RunProgress> = {}): RunProgress {
  return {
    total: 0,
    pending: 0,
    running: 0,
    done: 0,
    failed: 0,
    blocked: 0,
    skipped: 0,
    interrupted: 0,
    awaiting_confirmation: 0,
    ...overrides,
  }
}

export interface RunFixtureOptions extends Omit<Partial<RunSummary>, 'title' | 'created_at' | 'progress'> {
  /** Defaults to `Run <id>`; component specs pass `批次 <id>`. */
  title?: string
  /** Defaults to a fixed timestamp; component specs pass a recent one. */
  createdAt?: string
  /** Per-state progress overrides (on top of total 4 / done 1). */
  progress?: Partial<RunProgress>
}

/** One RunSummary row; active/history flags derive from the status. */
export function runFixture(id: string, status: RunSummary['status'], options: RunFixtureOptions = {}): RunSummary {
  const { title = `Run ${id}`, createdAt = '2026-08-19T09:00:00.000Z', progress: progressOverrides, ...overrides } = options
  return {
    id,
    title,
    status,
    active: ['pending', 'running', 'paused'].includes(status),
    history: ['completed', 'completed_with_failures', 'canceled'].includes(status),
    created_at: createdAt,
    progress: runProgress({ total: 4, done: 1, ...progressOverrides }),
    stalled: 0,
    ...overrides,
  }
}
