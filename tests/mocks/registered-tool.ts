/**
 * Shared tool-registration test skeleton: the structural RegisteredTool
 * face of defineTool output, a stub ctx that captures registrations, and
 * the tool lookup / render helpers used by the tool-layer specs.
 */
import { expect } from 'vitest'

/* eslint-disable @typescript-eslint/no-explicit-any */

export interface RegisteredTool {
  name: string
  execute: (args: any, exec?: any) => Promise<any>
  output: { render: (args: any, value: any) => { type: string; text?: string }[] }
}

/** ctx stub capturing every registered tool into a name-keyed map. */
export function stubToolCtx(tools: Map<string, RegisteredTool>) {
  return {
    tools: {
      register(def: RegisteredTool) {
        tools.set(def.name, def)
      },
    },
  }
}

/** Look up a registered tool (fails the test when missing). */
export function toolOf(tools: Map<string, RegisteredTool>, name: string): RegisteredTool {
  const found = tools.get(name)
  expect(found).toBeDefined()
  return found as RegisteredTool
}

/** Render a tool result to its joined model-facing text. */
export function renderToolText(tools: Map<string, RegisteredTool>, toolName: string, args: unknown, value: unknown): string {
  return toolOf(tools, toolName).output.render(args, value).map(block => block.text ?? '').join('\n')
}
