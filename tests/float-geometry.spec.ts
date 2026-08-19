/** Floating-window geometry tests (STORY-0006, TICKET-0036/0037). */
import { describe, expect, it } from 'vitest'
import { clampFloatPos, clampFloatSize, FLOAT_MIN_H, FLOAT_MIN_W } from '../src/client/float-geometry.ts'

describe('clampFloatSize', () => {
  it('keeps an in-range size untouched (rounded)', () => {
    expect(clampFloatSize({ w: 380, h: 520 }, 1440, 900)).toEqual({ w: 380, h: 520 })
    expect(clampFloatSize({ w: 380.4, h: 519.6 }, 1440, 900)).toEqual({ w: 380, h: 520 })
  })

  it('raises sub-minimum sizes to the minimum', () => {
    expect(clampFloatSize({ w: 100, h: 120 }, 1440, 900)).toEqual({ w: FLOAT_MIN_W, h: FLOAT_MIN_H })
  })

  it('caps the size at the viewport', () => {
    expect(clampFloatSize({ w: 2000, h: 1200 }, 1440, 900)).toEqual({ w: 1440, h: 900 })
  })

  it('prefers the viewport over the minimum on tiny windows', () => {
    // A 240px viewport cannot honor the 280px minimum without overflow.
    expect(clampFloatSize({ w: 400, h: 500 }, 240, 300)).toEqual({ w: FLOAT_MIN_W, h: 320 })
    expect(clampFloatSize({ w: 200, h: 200 }, 240, 300)).toEqual({ w: FLOAT_MIN_W, h: FLOAT_MIN_H })
  })
})

describe('clampFloatPos', () => {
  it('keeps an on-screen position untouched (rounded)', () => {
    expect(clampFloatPos({ x: 96, y: 96 }, 1440, 900)).toEqual({ x: 96, y: 96 })
    expect(clampFloatPos({ x: 96.4, y: 95.6 }, 1440, 900)).toEqual({ x: 96, y: 96 })
  })

  it('pulls negative coordinates back to the origin', () => {
    expect(clampFloatPos({ x: -50, y: -10 }, 1440, 900)).toEqual({ x: 0, y: 0 })
  })

  it('keeps the window reachable: 80px stays on screen horizontally', () => {
    expect(clampFloatPos({ x: 2000, y: 100 }, 1440, 900)).toEqual({ x: 1440 - 80, y: 100 })
  })

  it('keeps the header band on screen vertically', () => {
    expect(clampFloatPos({ x: 100, y: 1200 }, 1440, 900)).toEqual({ x: 100, y: 900 - 40 })
  })

  it('never produces a negative max on tiny viewports', () => {
    expect(clampFloatPos({ x: 500, y: 500 }, 60, 30)).toEqual({ x: 0, y: 0 })
  })
})
