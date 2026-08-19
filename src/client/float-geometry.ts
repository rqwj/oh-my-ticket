/**
 * Floating-window geometry: pure clamp helpers for the OMT float shell
 * (STORY-0006). Kept component-free so the logic is unit-testable and the
 * persisted preferences can be re-clamped against a CHANGED viewport at
 * render time (stored values may predate a window resize or monitor move).
 */

export interface FloatPos {
  readonly x: number
  readonly y: number
}

export interface FloatSize {
  readonly w: number
  readonly h: number
}

/** Minimum usable window: narrower/shorter and the filters stop fitting. */
export const FLOAT_MIN_W = 280
export const FLOAT_MIN_H = 320

/**
 * Reachability margin: after clamping, at least this much of the window
 * stays on screen horizontally (drag header stays grabbable) and the header
 * band stays on screen vertically.
 */
const KEEP_VISIBLE_X = 80
const KEEP_VISIBLE_Y = 40

/**
 * Clamp a size to [min, viewport]. A viewport smaller than the minimum
 * yields the viewport itself (better unusable-small than off-screen).
 */
export function clampFloatSize(size: FloatSize, viewportW: number, viewportH: number): FloatSize {
  const maxW = Math.max(viewportW, FLOAT_MIN_W)
  const maxH = Math.max(viewportH, FLOAT_MIN_H)
  return {
    w: Math.min(Math.max(Math.round(size.w), FLOAT_MIN_W), maxW),
    h: Math.min(Math.max(Math.round(size.h), FLOAT_MIN_H), maxH),
  }
}

/**
 * Clamp a position so the window never leaves the viewport: x keeps at
 * least KEEP_VISIBLE_X px on screen, y keeps the header band (top edge
 * within [0, viewportH - KEEP_VISIBLE_Y]).
 */
export function clampFloatPos(pos: FloatPos, viewportW: number, viewportH: number): FloatPos {
  return {
    x: Math.min(Math.max(Math.round(pos.x), 0), Math.max(viewportW - KEEP_VISIBLE_X, 0)),
    y: Math.min(Math.max(Math.round(pos.y), 0), Math.max(viewportH - KEEP_VISIBLE_Y, 0)),
  }
}
