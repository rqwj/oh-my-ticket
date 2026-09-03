/**
 * Runtime prerequisite state shared by native tools and run_code dispatches.
 * Pure policy lives here; Cordis hook/tool wiring is added below it.
 */
import { posix } from 'node:path'
import { defineTool } from '@deepseek-ai/dsh-tools'
import { pluginInstructionMessage } from './messages.ts'
import { boundSkillStages } from './prompt.ts'

export interface GateToolCall {
  sessionId: string
  rootCallId: string
  name: string
  arguments: unknown
}

export interface BoundSkillGateOptions {
  getBoundSkillNames: () => readonly string[]
  hasRunningNode: (sessionId: string) => boolean
  parentSessionOf?: (sessionId: string) => string | undefined
}

interface GateState {
  turn?: number
  step?: number
  readonly loaded: Set<string>
  readonly pendingLoaded: Set<string>
  readonly deliverySkills: Set<string>
  readonly deliveryRootsBySkill: Map<string, Set<string>>
  bypass?: true
  bypassUsed?: true
}

function newState(): GateState {
  return {
    loaded: new Set(),
    pendingLoaded: new Set(),
    deliverySkills: new Set(),
    deliveryRootsBySkill: new Map(),
  }
}

function filePathOf(value: unknown): string | undefined {
  if (value === null || typeof value !== 'object') return undefined
  const raw = (value as { file_path?: unknown }).file_path
  return typeof raw === 'string' ? raw : undefined
}

/** Planning writes remain possible before ticket creation without allowing `..` escapes. */
export function isPlanningArtifactPath(filePath: string): boolean {
  const normalized = posix.normalize(filePath.replaceAll('\\', '/'))
  const parts = normalized.split('/').filter(Boolean)
  const fileName = parts.at(-1)?.toLowerCase() ?? ''
  const extensionAllowed = fileName.endsWith('.md') || fileName.endsWith('.html')
  return extensionAllowed && parts.slice(0, -1).includes('plans')
}

export class BoundSkillGate {
  private readonly states = new Map<string, GateState>()
  private readonly options: BoundSkillGateOptions

  constructor(options: BoundSkillGateOptions) {
    this.options = options
  }

  onStep(sessionId: string, turn: number, step: number): void {
    const state = this.state(sessionId)
    if (state.turn !== turn) {
      this.states.set(sessionId, { ...newState(), turn, step })
      return
    }
    if (state.step !== step) {
      for (const name of state.pendingLoaded) state.loaded.add(name)
      state.pendingLoaded.clear()
      state.deliverySkills.clear()
      state.deliveryRootsBySkill.clear()
      state.step = step
    }
  }

  onSkillStart(sessionId: string, skillName: string, rootCallId: string): void {
    if (!this.options.getBoundSkillNames().includes(skillName)) return
    const state = this.state(sessionId)
    state.deliverySkills.add(skillName)
    const roots = state.deliveryRootsBySkill.get(skillName) ?? new Set<string>()
    roots.add(rootCallId)
    state.deliveryRootsBySkill.set(skillName, roots)
  }

  onSkillResult(sessionId: string, skillName: string, success: boolean): void {
    if (!success || !this.options.getBoundSkillNames().includes(skillName)) return
    this.state(sessionId).pendingLoaded.add(skillName)
  }

  armBypass(sessionId: string): void {
    const state = this.state(sessionId)
    if (state.bypassUsed !== undefined) throw new Error('omt_bypass was already used in this turn')
    state.bypass = true
    state.bypassUsed = true
  }

  forgetSession(sessionId: string): void {
    this.states.delete(sessionId)
  }

  guard(call: GateToolCall): string | undefined {
    if (call.name !== 'omt_create' && call.name !== 'edit' && call.name !== 'write') return undefined
    const bound = this.options.getBoundSkillNames()
    const stages = boundSkillStages(bound)
    if (stages.split.length === 0 && stages.implementation.length === 0) return undefined

    const state = this.state(call.sessionId)
    const filePath = filePathOf(call.arguments)
    const planningWrite = call.name !== 'omt_create' && filePath !== undefined && isPlanningArtifactPath(filePath)
    const deliveryStage = call.name === 'omt_create' || planningWrite ? stages.split : stages.implementation
    const pendingDelivery = deliveryStage.filter(name => state.deliverySkills.has(name))
    if (pendingDelivery.length > 0) {
      const skills = pendingDelivery.map(name => `\`${name}\``).join(', ')
      const roots = pendingDelivery.flatMap(name => [...(state.deliveryRootsBySkill.get(name) ?? [])]).join(', ')
      return `OMT delivery gate: ${skills} was invoked in this model step (root: ${roots}). Read-only work may continue, but end the current run_code/tool batch before ${call.name} so its instructions reach the model, then retry on the next model step.`
    }

    if (call.name === 'omt_create' || planningWrite) {
      const missing = stages.split.filter(name => !state.loaded.has(name))
      if (missing.length > 0) return `OMT prerequisite missing: load and follow ${missing.map(name => `\`${name}\``).join(', ')} before ${call.name}, then retry on the next model step.`
      return undefined
    }
    if (state.bypass !== undefined) {
      state.bypass = undefined
      return undefined
    }

    const missing = this.missingAcrossLineage(call.sessionId, stages.implementation)
    if (missing.length > 0) return `OMT prerequisite missing: load and follow ${missing.map(name => `\`${name}\``).join(', ')} before ${call.name}, then retry on the next model step.`
    if (!this.lineage(call.sessionId).some(sessionId => this.options.hasRunningNode(sessionId))) {
      return `OMT transition missing: no active model-owned in_progress node. Use omt_show/omt_create as needed, then omt_update the implementation node to in_progress before ${call.name}; for one truly trivial mutation, call omt_bypass with a reason.`
    }
    return undefined
  }

  private state(sessionId: string): GateState {
    let state = this.states.get(sessionId)
    if (state === undefined) {
      state = newState()
      this.states.set(sessionId, state)
    }
    return state
  }

  private missingAcrossLineage(sessionId: string, required: readonly string[]): string[] {
    const loaded = new Set<string>()
    for (const id of this.lineage(sessionId)) {
      for (const name of this.states.get(id)?.loaded ?? []) loaded.add(name)
    }
    return required.filter(name => !loaded.has(name))
  }

  private lineage(sessionId: string): string[] {
    const ids: string[] = []
    const seen = new Set<string>()
    let current: string | undefined = sessionId
    while (current !== undefined && !seen.has(current)) {
      seen.add(current)
      ids.push(current)
      current = this.options.parentSessionOf?.(current)
    }
    return ids
  }
}

interface HookAgentLike {
  id?: string
  session?: { header?: { id?: string; parentSession?: string } }
}

interface HookExecutionLike {
  agent?: HookAgentLike
  name: string
  arguments: unknown
  rootCallId: string
  parent?: unknown
}

interface HookResultLike {
  isError: boolean
  content?: readonly unknown[]
}

interface HookContextLike {
  on(name: string, listener: (...args: any[]) => Promise<any>): unknown
}

function sessionIdOf(agent: HookAgentLike | undefined): string | undefined {
  return agent?.session?.header?.id ?? agent?.id
}

function invokedSkillName(exec: HookExecutionLike): string | undefined {
  if (exec.name !== 'skill' || exec.arguments === null || typeof exec.arguments !== 'object') return undefined
  const name = (exec.arguments as { name?: unknown }).name
  return typeof name === 'string' ? name : undefined
}

/** Register only lifecycle hooks; guard and bypass tool wiring stay separate. */
export function registerSkillGateHooks(ctx: HookContextLike, gate: BoundSkillGate): void {
  ctx.on('agent/pre-step', async (payload: { agent: HookAgentLike; turn: number; step: number }, next: () => Promise<any>) => {
    const downstream = await next()
    const sessionId = sessionIdOf(payload.agent)
    if (sessionId !== undefined && downstream?.kind === 'enter') gate.onStep(sessionId, payload.turn, payload.step)
    return downstream
  })

  ctx.on('tools/pre-execute', async (exec: HookExecutionLike, next: () => Promise<any>) => {
    const sessionId = sessionIdOf(exec.agent)
    const skillName = invokedSkillName(exec)
    if (sessionId !== undefined && skillName !== undefined) gate.onSkillStart(sessionId, skillName, exec.rootCallId)
    return await next()
  })

  ctx.on('agent/disposed', async (payload: { agent: HookAgentLike }) => {
    const sessionId = sessionIdOf(payload.agent)
    if (sessionId !== undefined) gate.forgetSession(sessionId)
  })

  ctx.on('tools/post-execute', async (exec: HookExecutionLike, result: HookResultLike, next: () => Promise<any>) => {
    const downstream = await next()
    const sessionId = sessionIdOf(exec.agent)
    const skillName = invokedSkillName(exec)
    const accepted = !result.isError && downstream.kind !== 'block'
    if (sessionId === undefined || skillName === undefined || !accepted) return downstream
    gate.onSkillResult(sessionId, skillName, true)
    if (exec.parent === undefined || result.content === undefined) return downstream
    return {
      ...downstream,
      additionalContexts: [pluginInstructionMessage(result.content), ...(downstream.additionalContexts ?? [])],
    }
  })
}

interface RuntimeContextLike extends HookContextLike {
  tools: {
    register(tool: unknown): unknown
    guard(guard: (exec: HookExecutionLike) => string | undefined): unknown
  }
}

interface GateAgentRegistryLike {
  get(id: string): HookAgentLike | undefined
}

interface GateRunningLike {
  forSession(sessionId: string): readonly unknown[]
}

export interface RegisterOmtSkillGateOptions {
  getBoundSkillNames: () => readonly string[]
  running: GateRunningLike
  agents?: GateAgentRegistryLike
}

/** Wire the state machine, monotonic guard, and explicit one-shot trivial bypass. */
export function registerOmtSkillGate(ctxValue: unknown, options: RegisterOmtSkillGateOptions): BoundSkillGate {
  const ctx = ctxValue as RuntimeContextLike
  const gate = new BoundSkillGate({
    getBoundSkillNames: options.getBoundSkillNames,
    hasRunningNode: sessionId => options.running.forSession(sessionId).length > 0,
    parentSessionOf: sessionId => options.agents?.get(sessionId)?.session?.header?.parentSession,
  })
  registerSkillGateHooks(ctx, gate)
  ctx.tools.guard(exec => {
    const sessionId = sessionIdOf(exec.agent)
    if (sessionId === undefined) return undefined
    return gate.guard({
      sessionId,
      rootCallId: String(exec.rootCallId),
      name: exec.name,
      arguments: exec.arguments,
    })
  })
  ctx.tools.register(defineTool({
    name: 'omt_bypass',
    description: '显式记录琐碎修改原因，并仅放行当前 DSH turn 内下一次 edit/write 尝试；不绕过 skill 内容交付锁或 omt_create 前置条件。',
    parameters: {
      reason: { type: 'string', required: true, description: '为何该变更确属无需拆票的琐碎单步修改' },
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          reason: { type: 'string', required: true },
          remaining: { type: 'integer', required: true },
        },
      },
      render: (_args, value: { reason: string; remaining: number }) => [{
        type: 'text',
        text: `已记录 OMT 琐碎修改 bypass：${value.reason}（剩余 ${value.remaining} 次 edit/write 尝试）`,
      }],
    },
    async execute(args, exec) {
      const sessionId = sessionIdOf(exec.agent)
      if (sessionId === undefined) throw new Error('omt_bypass requires an agent session')
      const reason = args.reason.trim()
      if (reason === '') throw new Error('omt_bypass requires a non-empty reason')
      gate.armBypass(sessionId)
      return { reason, remaining: 1 }
    },
  }))
  return gate
}
