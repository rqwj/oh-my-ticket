/**
 * OMT browser half. M5: assembles the client plugin — M4 '@' trigger source,
 * the ticket tree in its three presentations (left drawer and floating
 * window on shell.overlay, OMT tab on conversation.view; STORY-0006), toggle
 * buttons (header + sidebar footer), the details-panel document shadow
 * (dynamic register/dispose), the active ticket dock strip, and the omt_show
 * keyed toolview. Typing is structural (see trigger/source.ts header note);
 * reactive facts flow through the inject hooks compartment per the slot
 * discipline.
 */
import { createTicketSource, type InputTriggerSource, type RpcCaller } from './trigger/source.ts'
import { OmtController, type LayoutLike } from './controller.ts'
import { Drawer } from './components/Drawer.tsx'
import { FloatWindow } from './components/FloatWindow.tsx'
import { TicketTab } from './components/TicketTab.tsx'
import { DocPanel } from './components/DocPanel.tsx'
import { ActiveDock } from './components/ActiveDock.tsx'
import { ReferencedBar } from './components/ReferencedBar.tsx'
import { ToggleButton } from './components/ToggleButton.tsx'
import { OmtShowRow } from './components/OmtShowRow.tsx'
import { TurnTickets } from './components/TurnTickets.tsx'
import { PromptSettings } from './components/PromptSettings.tsx'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-runtime/client'
import { PromptSettingsModel, INITIAL_PROMPT_SETTINGS_VIEW } from './prompt-settings-model.ts'
import type { BoundSkillRow } from '../host/prompt-settings.ts'
import { en, NS, zh, type Translate } from './locales.ts'
// Shared tokens + badge/dot/type/status classes (single injected sheet).
import './omt-shared.css'

interface SlotsLike {
  inject(name: string, contribute: () => unknown): () => void
  register(definition: Record<string, unknown>, component: unknown): () => void
}

/** Locale dictionary service (dsh-client-locale; structural subset). */
interface LocaleLike {
  register(ns: string, dicts: Record<string, Record<string, string>>): () => void
  /** Bound translate for one namespace (stable identity, live lookups). */
  bind(ns: string): (key: string, params?: Record<string, unknown>) => string
}

interface ClientContextLike {
  slots: SlotsLike
  connection: { rpc: RpcCaller }
  inputTriggers: { registerSource(source: InputTriggerSource): () => void }
  layout: LayoutLike
  locale: LocaleLike
}

export const inject = ['slots', 'connection', 'inputTriggers', 'layout', 'locale']

export function apply(ctx: ClientContextLike): void {
  const controller = new OmtController(ctx.connection.rpc, ctx.layout)
  controller.connectEvents()

  // zh/en dictionaries for every 'omt' slot entry (locale: NS below puts the
  // framework-synthesized `t` seat on each component's props).
  ctx.locale.register(NS, { zh, en })

  // M4: '@' ticket reference source; successful serializations feed the
  // turn-tail related list. The bound translate localizes the serialized
  // block's labels (they land in the submitted message).
  ctx.inputTriggers.registerSource(
    createTicketSource(ctx.connection.rpc, (sessionId, summary) => {
      if (sessionId !== undefined) controller.noteRelated(sessionId, [summary])
    }, ctx.locale.bind(NS) as Translate),
  )

  // Tree overlay presentations (STORY-0006): the drawer and the floating
  // window share the panel-open fact (drawerOpen) and the panelMode gate —
  // exactly one renders at a time, so switching modes mid-session swaps the
  // shell while the tree/filter state carries over. The shared inject
  // compartment below feeds both shells.
  const overlayInject = () => ({
    hooks: {
      drawerOpen: controller.drawerOpen,
      panelMode: controller.panelMode,
      tree: controller.tree,
      active: controller.active,
      drawerWidth: controller.drawerWidth,
      collapsed: controller.collapsed,
      floatPos: controller.floatPos,
      floatSize: controller.floatSize,
    },
    setDrawerWidth: controller.setDrawerWidth,
    setFloatPos: controller.setFloatPos,
    setFloatSize: controller.setFloatSize,
    setPanelMode: controller.setPanelMode,
    toggleCollapsed: controller.toggleCollapsed,
    toggleDrawer: controller.toggleDrawer,
    refreshTree: controller.refreshTree,
    reindex: controller.reindex,
    select: controller.select,
    archive: (id: string, sessionId?: string) => controller.setArchived(id, true, sessionId),
    createNode: controller.createNode,
    expandIds: controller.expandIds,
  })

  // Left side drawer (frame-wide overlay).
  ctx.slots.inject('shell.overlay', () =>
    ctx.slots.register({
      name: 'shell.overlay',
      id: 'omt-drawer',
      order: 50,
      locale: NS,
      inject: overlayInject,
    }, Drawer))

  // Floating window (frame-wide overlay, mode-gated against the drawer).
  ctx.slots.inject('shell.overlay', () =>
    ctx.slots.register({
      name: 'shell.overlay',
      id: 'omt-float',
      order: 51,
      locale: NS,
      inject: overlayInject,
    }, FloatWindow))

  // Third presentation: the OMT tab in the conversation view ring, beside
  // Chat | Trajectory (session scope — the framework hands over sessionId).
  // Registered dynamically (TICKET-0040): the controller disposes the entry
  // while the floating window owns the ticket list, dropping the tab from
  // the ring; the shell falls back to Chat for any session staged on it.
  controller.attachViewTab(() =>
    ctx.slots.inject('conversation.view', () =>
      ctx.slots.register({
        name: 'conversation.view',
        id: 'omt',
        order: 20,
        label: 'OMT',
        locale: NS,
        inject: () => ({
          hooks: {
            tree: controller.tree,
            active: controller.active,
            collapsed: controller.collapsed,
          },
          toggleCollapsed: controller.toggleCollapsed,
          refreshTree: controller.refreshTree,
          reindex: controller.reindex,
          select: controller.select,
          archive: (id: string, sessionId?: string) => controller.setArchived(id, true, sessionId),
          createNode: controller.createNode,
          expandIds: controller.expandIds,
          setPanelMode: controller.setPanelMode,
          openPanel: controller.openPanel,
        }),
      }, TicketTab)))

  // Drawer toggle buttons: session header + sidebar footer.
  const toggleInject = () => ({
    hooks: { drawerOpen: controller.drawerOpen },
    toggleDrawer: controller.toggleDrawer,
    noteSession: controller.noteSession,
  })
  ctx.slots.inject('conversation.session.header.actions', () =>
    ctx.slots.register(
      { name: 'conversation.session.header.actions', id: 'omt-toggle', order: 90, locale: NS, inject: toggleInject },
      ToggleButton,
    ))
  ctx.slots.inject('sidebar.footer.action', () =>
    ctx.slots.register(
      { name: 'sidebar.footer.action', id: 'omt-toggle', order: 90, locale: NS, inject: toggleInject },
      ToggleButton,
    ))

  // Referenced tickets strip (full titles, clickable → details panel).
  ctx.slots.inject('conversation.input.dock', () =>
    ctx.slots.register({
      name: 'conversation.input.dock',
      id: 'omt-refs',
      order: 40,
      locale: NS,
      inject: () => ({
        hooks: { summaries: controller.summaries },
        ensureSummaries: controller.ensureSummaries,
        select: controller.select,
      }),
    }, ReferencedBar))

  // Active ticket strip above the composer.
  ctx.slots.inject('conversation.input.dock', () =>
    ctx.slots.register({
      name: 'conversation.input.dock',
      id: 'omt-active',
      order: 50,
      locale: NS,
      inject: () => ({
        hooks: { active: controller.active },
        select: controller.select,
        clearActive: controller.clearActive,
      }),
    }, ActiveDock))

  // Details-panel document shadow: registered while a doc is active,
  // disposed on close (restoring the stock tool-details panel).
  controller.attachDetailsShadow(() =>
    ctx.slots.inject('details', () =>
      ctx.slots.register({
        name: 'details',
        priority: -10,
        locale: NS,
        inject: () => ({
          hooks: { doc: controller.doc },
          executeTicket: controller.executeTicket,
          closeDoc: controller.closeDoc,
          setStatus: controller.setStatus,
          setArchived: controller.setArchived,
          rename: controller.rename,
          setPriority: controller.setPriority,
          appendNote: controller.appendNote,
          saveBody: controller.saveBody,
          setBodyEditing: controller.setBodyEditing,
          select: controller.select,
          forget: controller.forget,
        }),
      }, DocPanel)))

  // omt_show tool result renders as a markdown document.
  ctx.slots.inject('tool.call.toolview', () =>
    ctx.slots.register({ name: 'tool.call.toolview', key: 'omt_show', locale: NS }, OmtShowRow))

  // Turn-tail related tickets. Chain is first-match-wins: we trail the
  // deliverables entry (default priority 0) so produced files keep priority;
  // our selector matches only when the staged session HAS related tickets.
  // The selector reads a hot snapshot (same discipline as the trigger
  // pipeline's space adjudication); TurnTailNodeView re-renders per turn,
  // so the answer is always fresh at the moment it matters.
  ctx.slots.inject('conversation.chat.turnTail', () =>
    ctx.slots.register({
      name: 'conversation.chat.turnTail',
      priority: 20,
      locale: NS,
      // Always claim (priority 20 still trails deliverables). Session
      // attribution MUST NOT come from a global "current session" here —
      // the matched share is frozen at election time, so a side channel
      // misattributes tails on session switch (TICKET-0021). The component
      // reads its own framework sessionId prop instead.
      select: () => ({}),
      inject: () => ({
        hooks: { related: controller.related },
        select: controller.select,
        refreshRelated: controller.refreshRelated,
      }),
    }, TurnTickets))

  const promptView = createSnapshotStore(INITIAL_PROMPT_SETTINGS_VIEW)
  const settingsScope = (ctx as unknown as {
    settingsScope?: { bind: (ns: string) => { set: (key: string, value: unknown) => Promise<void> } }
  }).settingsScope
  const promptModel = new PromptSettingsModel(
    async () => {
      const result = await ctx.connection.rpc.call('/omt', 'skills', {})
      if (!result.ok) throw new Error(result.error.message)
      const value = result.value as { extraPrompt: string; skills: BoundSkillRow[] }
      return { extraPrompt: value.extraPrompt, skills: value.skills }
    },
    async next => {
      if (settingsScope === undefined) throw new Error('settings unavailable')
      const bound = settingsScope.bind('oh-my-ticket-prompt')
      await bound.set('extraPrompt', next.extraPrompt)
      await bound.set('boundSkillNames', next.boundSkillNames)
    },
    view => { promptView.set(view) },
  )
  void promptModel.load()
  ctx.slots.inject('settings.section', () =>
    ctx.slots.register({
      name: 'settings.section',
      id: 'omt-prompt',
      order: 80,
      locale: NS,
      inject: () => ({
        hooks: { view: promptView },
        setDraftExtra: promptModel.setDraftExtra.bind(promptModel),
        setExtraPrompt: (value: string) => { void promptModel.setExtraPrompt(value) },
        toggle: (name: string) => { void promptModel.toggle(name) },
        retry: () => { void promptModel.load() },
      }),
    }, PromptSettings))
}
