/** Narrow-viewport drawer geometry tests (TICKET-0033). */
import { describe, expect, it } from 'vitest'
import { effectiveDrawerWidth, isNarrowViewport } from '../src/client/components/Drawer.tsx'

describe('effectiveDrawerWidth', () => {
  it('honors the stored preference on wide viewports', () => {
    expect(effectiveDrawerWidth(320, 1440)).toBe(320)
    expect(effectiveDrawerWidth(480, 1440)).toBe(480)
  })

  it('caps the preference at the viewport (min(width, 100vw))', () => {
    // Stored 480 on a 400px window must not overflow the screen.
    expect(effectiveDrawerWidth(480, 400)).toBe(400)
  })

  it('covers the full width below the narrow breakpoint', () => {
    expect(effectiveDrawerWidth(480, 320)).toBe(320)
    expect(effectiveDrawerWidth(240, 639)).toBe(639)
  })

  it('hands control back at the breakpoint edge', () => {
    expect(effectiveDrawerWidth(480, 640)).toBe(480)
  })
})

describe('isNarrowViewport', () => {
  it('flags only sub-breakpoint viewports (drag handle retired)', () => {
    expect(isNarrowViewport(320)).toBe(true)
    expect(isNarrowViewport(639)).toBe(true)
    expect(isNarrowViewport(640)).toBe(false)
    expect(isNarrowViewport(1440)).toBe(false)
  })
})
