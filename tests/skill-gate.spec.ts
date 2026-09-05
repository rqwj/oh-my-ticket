import { describe, expect, it } from 'vitest'
import { BoundSkillGate, isPlanningArtifactPath, registerOmtSkillGate, registerSkillGateHooks } from '../src/host/skill-gate.ts'
import { boundSkillStages } from '../src/host/prompt.ts'

function makeGate(bound: readonly string[], running: Set<string> = new Set(), parents: Record<string, string | undefined> = {}) {
  return new BoundSkillGate({
    getBoundSkillNames: () => bound,
    hasRunningNode: sessionId => running.has(sessionId),
    parentSessionOf: sessionId => parents[sessionId],
  })
}

describe('boundSkillStages', () => {
  it('shares the prompt skill buckets with runtime policy', () => {
    expect(boundSkillStages(['ce-brainstorm', 'ce-plan', 'ce-work', 'ce-worktree', 'ce-test-browser', 'omt'])).toEqual({
      split: ['ce-brainstorm', 'ce-plan'],
      implementation: ['ce-work', 'ce-worktree'],
      verification: ['ce-test-browser'],
      other: ['omt'],
    })
  })
})

describe('isPlanningArtifactPath', () => {
  it('allows normalized markdown and html below a plans ancestor', () => {
    expect(isPlanningArtifactPath('docs/plans/feature.md')).toBe(true)
    expect(isPlanningArtifactPath('/tmp/work/plans/feature.html')).toBe(true)
  })

  it('rejects traversal escapes and arbitrary markdown', () => {
    expect(isPlanningArtifactPath('docs/plans/../notes.md')).toBe(false)
    expect(isPlanningArtifactPath('docs/notes.md')).toBe(false)
    expect(isPlanningArtifactPath('docs/plans/code.ts')).toBe(false)
  })
})

describe('registerSkillGateHooks', () => {
  it('ferries complete nested skill content and promotes credit on the following step', async () => {
    const listeners = new Map<string, (...args: any[]) => Promise<any>>()
    const ctx = {
      on(name: string, listener: (...args: any[]) => Promise<any>) {
        listeners.set(name, listener)
      },
    }
    const gate = makeGate(['ce-work'], new Set(['session-a']))
    registerSkillGateHooks(ctx, gate)
    const agent = { id: 'session-a', session: { header: { id: 'session-a' } } }
    const nextStep = async () => ({ kind: 'enter', messages: [] })

    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 0 }, nextStep)
    await listeners.get('tools/pre-execute')!({ agent, name: 'skill', arguments: { name: 'ce-work' }, rootCallId: 'run-1', parent: {} }, async () => ({ kind: 'allow' }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'run-1', name: 'write', arguments: { file_path: 'src/a.ts' } })).toContain('next model step')

    const existing = { id: 'existing', role: 'user', content: [{ type: 'text', text: 'existing' }], source: { kind: 'plugin', plugin: 'other' } }
    const decision = await listeners.get('tools/post-execute')!(
      { agent, name: 'skill', arguments: { name: 'ce-work' }, rootCallId: 'run-1', parent: {} },
      { isError: false, content: [{ type: 'text', text: 'FULL SKILL CONTENT' }], value: { name: 'ce-work' } },
      async () => ({ kind: 'accept', additionalContexts: [existing] }),
    )
    expect(decision.additionalContexts).toHaveLength(2)
    expect(decision.additionalContexts[0].content).toEqual([{ type: 'text', text: 'FULL SKILL CONTENT' }])
    expect(decision.additionalContexts[0].source).toMatchObject({ kind: 'plugin', plugin: 'oh-my-ticket', form: 'instructions' })
    expect(decision.additionalContexts[1]).toBe(existing)

    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 1 }, nextStep)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'write', arguments: { file_path: 'src/a.ts' } })).toBeUndefined()
  })

  it('does not credit failed or blocked skill results and only promotes after an accepted pre-step', async () => {
    const listeners = new Map<string, (...args: any[]) => Promise<any>>()
    const ctx = { on: (name: string, listener: (...args: any[]) => Promise<any>) => listeners.set(name, listener) }
    const gate = makeGate(['ce-plan'])
    registerSkillGateHooks(ctx, gate)
    const agent = { id: 'session-a', session: { header: { id: 'session-a' } } }
    const skillExec = { agent, name: 'skill', arguments: { name: 'ce-plan' }, rootCallId: 'skill-1' }

    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 0 }, async () => ({ kind: 'enter', messages: [] }))
    await listeners.get('tools/pre-execute')!({ ...skillExec, arguments: { name: 'ce-work' } }, async () => ({ kind: 'allow' }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-0', name: 'omt_create', arguments: {} })).toContain('ce-plan')

    await listeners.get('tools/pre-execute')!(skillExec, async () => ({ kind: 'allow' }))
    await listeners.get('tools/post-execute')!(skillExec, { isError: true, content: [] }, async () => ({ kind: 'accept' }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'omt_create', arguments: {} })).toContain('next model step')
    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 1 }, async () => ({ kind: 'enter', messages: [] }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'omt_create', arguments: {} })).toContain('ce-plan')

    await listeners.get('tools/pre-execute')!(skillExec, async () => ({ kind: 'allow' }))
    await listeners.get('tools/post-execute')!(skillExec, { isError: false, content: [] }, async () => ({ kind: 'block' }))
    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 2 }, async () => ({ kind: 'enter', messages: [] }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-3', name: 'omt_create', arguments: {} })).toContain('ce-plan')

    await listeners.get('tools/pre-execute')!(skillExec, async () => ({ kind: 'allow' }))
    await listeners.get('tools/post-execute')!(skillExec, { isError: false, content: [] }, async () => ({ kind: 'accept' }))
    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 3 }, async () => ({ kind: 'reject' }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-4', name: 'omt_create', arguments: {} })).toContain('next model step')
    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 3 }, async () => ({ kind: 'enter', messages: [] }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-5', name: 'omt_create', arguments: {} })).toBeUndefined()
  })

  it('does not duplicate native skill content but delays its load credit', async () => {
    const listeners = new Map<string, (...args: any[]) => Promise<any>>()
    const ctx = { on: (name: string, listener: (...args: any[]) => Promise<any>) => listeners.set(name, listener) }
    const gate = makeGate(['ce-plan'])
    registerSkillGateHooks(ctx, gate)
    const agent = { id: 'session-a', session: { header: { id: 'session-a' } } }

    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 0 }, async () => ({ kind: 'enter', messages: [] }))
    await listeners.get('tools/pre-execute')!({ agent, name: 'skill', arguments: { name: 'ce-plan' }, rootCallId: 'skill-1' }, async () => ({ kind: 'allow' }))
    const decision = await listeners.get('tools/post-execute')!(
      { agent, name: 'skill', arguments: { name: 'ce-plan' }, rootCallId: 'skill-1' },
      { isError: false, content: [{ type: 'text', text: 'PLAN' }], value: {} },
      async () => ({ kind: 'accept' }),
    )
    expect(decision.additionalContexts).toBeUndefined()
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'omt_create', arguments: {} })).toContain('next model step')
    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 1 }, async () => ({ kind: 'enter', messages: [] }))
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'omt_create', arguments: {} })).toBeUndefined()
    await listeners.get('agent/disposed')!({ agent })
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-3', name: 'omt_create', arguments: {} })).toContain('ce-plan')
  })
})

describe('registerOmtSkillGate', () => {
  it('registers the monotonic guard and an auditable one-shot bypass tool', async () => {
    const listeners = new Map<string, (...args: any[]) => Promise<any>>()
    const guards: Array<(exec: any) => string | undefined> = []
    const tools = new Map<string, any>()
    const ctx = {
      on: (name: string, listener: (...args: any[]) => Promise<any>) => listeners.set(name, listener),
      tools: {
        guard: (guard: (exec: any) => string | undefined) => guards.push(guard),
        register: (tool: any) => tools.set(tool.name, tool),
      },
    }
    const agent = { id: 'session-a', session: { header: { id: 'session-a' } } }
    registerOmtSkillGate(ctx, {
      getBoundSkillNames: () => ['ce-plan', 'ce-work'],
      running: { forSession: () => [] },
      agents: { get: () => agent },
    })
    await listeners.get('agent/pre-step')!({ agent, turn: 1, step: 0 }, async () => ({ kind: 'enter', messages: [] }))

    const bypass = tools.get('omt_bypass')
    expect(bypass).toBeDefined()
    await expect(bypass.execute({ reason: '   ' }, { agent })).rejects.toThrow('non-empty reason')
    await expect(bypass.execute({ reason: ' single typo ' }, { agent })).resolves.toMatchObject({ reason: 'single typo', remaining: 1 })
    expect(guards[0]!({ agent, rootCallId: 'create-1', name: 'omt_create', arguments: {} })).toContain('ce-plan')
    const exec = { agent, rootCallId: 'call-1', name: 'write', arguments: { file_path: 'src/a.ts' } }
    expect(guards[0]!(exec)).toBeUndefined()
    expect(guards[0]!({ ...exec, rootCallId: 'call-2' })).toContain('ce-work')
    await expect(bypass.execute({ reason: ' second typo ' }, { agent })).rejects.toThrow('already used in this turn')
  })
})

describe('BoundSkillGate policy', () => {
  it('preserves existing behavior when no split or implementation skills are bound', () => {
    const gate = makeGate(['omt', 'ce-test-browser'])
    gate.onStep('session-a', 1, 0)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'omt_create', arguments: {} })).toBeUndefined()
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'write', arguments: { file_path: 'src/a.ts' } })).toBeUndefined()
  })

  it('allows only normalized planning artifacts before ticket execution', () => {
    const gate = makeGate(['ce-work'])
    gate.onStep('session-a', 1, 0)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'write', arguments: { file_path: 'docs/plans/feature.md' } })).toBeUndefined()
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'edit', arguments: { file_path: 'docs/plans/feature.html' } })).toBeUndefined()
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-3', name: 'write', arguments: { file_path: 'docs/notes.md' } })).toContain('ce-work')
  })

  it('requires configured split skills before writing planning artifacts', () => {
    const gate = makeGate(['ce-plan', 'ce-work'])
    gate.onStep('session-a', 1, 0)
    const planningWrite = { sessionId: 'session-a', rootCallId: 'call-1', name: 'write', arguments: { file_path: 'docs/plans/feature.md' } }
    expect(gate.guard(planningWrite)).toContain('ce-plan')
    gate.onSkillStart('session-a', 'ce-plan', 'skill-1')
    gate.onSkillResult('session-a', 'ce-plan', true)
    gate.onStep('session-a', 1, 1)
    expect(gate.guard({ ...planningWrite, rootCallId: 'call-2' })).toBeUndefined()
  })

  it('requires split skills for omt_create only after their content reaches the next step', () => {
    const gate = makeGate(['ce-brainstorm', 'ce-plan'])
    gate.onStep('session-a', 1, 0)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'omt_create', arguments: {} })).toContain('ce-brainstorm')

    gate.onSkillStart('session-a', 'ce-brainstorm', 'run-1')
    gate.onSkillResult('session-a', 'ce-brainstorm', true)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'run-1', name: 'omt_create', arguments: {} })).toContain('next model step')

    gate.onStep('session-a', 1, 1)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'omt_create', arguments: {} })).toContain('ce-plan')
    gate.onSkillStart('session-a', 'ce-plan', 'run-2')
    gate.onSkillResult('session-a', 'ce-plan', true)
    gate.onStep('session-a', 1, 2)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-3', name: 'omt_create', arguments: {} })).toBeUndefined()
  })

  it('locks only the mutation type governed by the skill stage', () => {
    const gate = makeGate(['ce-plan', 'ce-work'], new Set(['session-a']))
    gate.onStep('session-a', 1, 0)
    gate.onSkillStart('session-a', 'ce-work', 'implementation-skill-1')
    gate.onSkillResult('session-a', 'ce-work', true)
    gate.onStep('session-a', 1, 1)

    gate.onSkillStart('session-a', 'ce-plan', 'split-skill')
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'create-1', name: 'omt_create', arguments: {} })).toContain('OMT delivery gate')
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'plan-1', name: 'write', arguments: { file_path: 'docs/plans/a.md' } })).toContain('OMT delivery gate')
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'write-1', name: 'write', arguments: { file_path: 'src/a.ts' } })).toBeUndefined()

    gate.onSkillResult('session-a', 'ce-plan', true)
    gate.onStep('session-a', 1, 2)
    gate.onSkillStart('session-a', 'ce-work', 'implementation-skill-2')
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'write-2', name: 'write', arguments: { file_path: 'src/a.ts' } })).toContain('OMT delivery gate')
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'create-2', name: 'omt_create', arguments: {} })).toBeUndefined()
  })

  it('does not arm mutation delivery locks for verification or other bound skills', () => {
    const gate = makeGate(['ce-plan', 'ce-work', 'ce-test-browser', 'omt'])
    gate.onStep('session-a', 1, 0)

    for (const skill of ['ce-test-browser', 'omt']) {
      gate.onSkillStart('session-a', skill, `${skill}-call`)
      const createDecision = gate.guard({ sessionId: 'session-a', rootCallId: 'create', name: 'omt_create', arguments: {} })
      expect(createDecision).toContain('OMT prerequisite missing')
      expect(createDecision).not.toContain('OMT delivery gate')
      const planningDecision = gate.guard({ sessionId: 'session-a', rootCallId: 'write', name: 'write', arguments: { file_path: 'docs/plans/a.md' } })
      expect(planningDecision).toContain('OMT prerequisite missing')
      expect(planningDecision).not.toContain('OMT delivery gate')
    }
  })

  it('requires an active execution marker and all bound implementation skills', () => {
    const running = new Set<string>(['session-a'])
    const gate = makeGate(['ce-work', 'ce-worktree'], running)
    gate.onStep('session-a', 1, 0)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'edit', arguments: { file_path: 'src/a.ts' } })).toContain('ce-work')

    for (const [index, skill] of ['ce-work', 'ce-worktree'].entries()) {
      gate.onSkillStart('session-a', skill, `skill-${index}`)
      gate.onSkillResult('session-a', skill, true)
      gate.onStep('session-a', 1, index + 1)
    }
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'edit', arguments: { file_path: 'src/a.ts' } })).toBeUndefined()
  })

  it('does not inherit split-stage skill credit from a parent session', () => {
    const gate = makeGate(['ce-plan'], new Set(), { child: 'root' })
    gate.onStep('root', 3, 0)
    gate.onSkillStart('root', 'ce-plan', 'skill-root')
    gate.onSkillResult('root', 'ce-plan', true)
    gate.onStep('root', 3, 1)
    gate.onStep('child', 1, 0)
    expect(gate.guard({ sessionId: 'child', rootCallId: 'call-1', name: 'omt_create', arguments: {} })).toContain('ce-plan')
  })

  it('walks full parent lineage for loaded skills and running ownership', () => {
    const running = new Set<string>(['root'])
    const gate = makeGate(['ce-work'], running, { child: 'root', grandchild: 'child' })
    gate.onStep('root', 3, 0)
    gate.onSkillStart('root', 'ce-work', 'skill-root')
    gate.onSkillResult('root', 'ce-work', true)
    gate.onStep('root', 3, 1)
    gate.onStep('grandchild', 1, 0)

    expect(gate.guard({ sessionId: 'grandchild', rootCallId: 'call-1', name: 'write', arguments: { file_path: 'src/a.ts' } })).toBeUndefined()
  })

  it('does not consume bypass while a same-step skill delivery lock blocks the mutation', () => {
    const gate = makeGate(['ce-work'])
    gate.onStep('session-a', 1, 0)
    gate.armBypass('session-a')
    gate.onSkillStart('session-a', 'ce-work', 'skill-1')
    const write = { sessionId: 'session-a', rootCallId: 'call-1', name: 'write', arguments: { file_path: 'src/a.ts' } }
    expect(gate.guard(write)).toContain('OMT delivery gate')
    gate.onSkillResult('session-a', 'ce-work', true)
    gate.onStep('session-a', 1, 1)
    expect(gate.guard({ ...write, rootCallId: 'call-2' })).toBeUndefined()
    expect(gate.guard({ ...write, rootCallId: 'call-3' })).toContain('no active model-owned')
  })

  it('resets loaded skills and bypass only when the DSH turn changes', () => {
    const running = new Set<string>(['session-a'])
    const gate = makeGate(['ce-work'], running)
    gate.onStep('session-a', 1, 0)
    gate.onSkillStart('session-a', 'ce-work', 'skill-1')
    gate.onSkillResult('session-a', 'ce-work', true)
    gate.onStep('session-a', 1, 1)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-1', name: 'write', arguments: { file_path: 'src/a.ts' } })).toBeUndefined()

    gate.armBypass('session-a')
    gate.onStep('session-a', 1, 2)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-2', name: 'write', arguments: { file_path: 'src/b.ts' } })).toBeUndefined()
    gate.onStep('session-a', 2, 0)
    expect(gate.guard({ sessionId: 'session-a', rootCallId: 'call-3', name: 'write', arguments: { file_path: 'src/c.ts' } })).toContain('ce-work')
  })
})
