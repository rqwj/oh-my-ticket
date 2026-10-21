/**
 * U5 (R-E): TranscriptView —— 选中 agent 最新 exec 的尾窗渲染。
 *
 * 虚拟化为手写实现（repo 无滚动依赖，不新增 npm 包）：固定行高 + 视口
 * 窗口切片（visibleRange 纯函数），滚动/事件到达时重算。行高固定换取
 * 简单可靠的切片语义，raw 行不做 Markdown 渲染（可变高度会破坏窗口
 * 坐标）；stdout/stderr 用样式区分，session_start/exit 结构化呈现。
 *
 * 行高 22px 不变量（验收 F-ACC-5 回归：行内多行文本曾互相叠印）：虚拟化
 * 切片单位是**渲染行**而非事件——flattenEvents 把每个事件展开为单条文本
 * 行（多行 prompt 拆行、`\r` 按换行处理、ANSI 转义/控制符剥除），CSS 的
 * `white-space: pre` 不再有产生高于 22px 的行的输入。
 *
 * 自动跟随（原 §九-13）：视口近底（FOLLOW_THRESHOLD_PX）才跟随新事件；
 * 上滚解除跟随并显示「回到最新 (N)」pill，点击回贴底恢复。live 且长时间
 * 无输出（IDLE_THRESHOLD_MS）显示「长时间无输出」提示（§3.2）。
 */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { TranscriptEvent } from './bridge'
import type { AgentRecord } from './types'
import { agentDisplayName, ConfirmStopButton } from './agentUi'
import { MarkdownText } from './MarkdownText'
import {
  IconBolt,
  IconChat,
  IconDoc,
  IconPlay,
  IconStopSquare,
  IconThink,
  IconTool,
  IconWarn,
} from './icons'

export const ROW_HEIGHT_PX = 22
export const OVERSCAN_ROWS = 10
export const FOLLOW_THRESHOLD_PX = 40
export const IDLE_THRESHOLD_MS = 60_000
/** live 期间 idle 提示的重估周期（无新事件不触发渲染，必须定时强制重估） */
export const IDLE_RECHECK_MS = 15_000

/** 视口近底判定（纯函数——jsdom 无布局也能测跟随语义）。 */
export function shouldFollow(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
  threshold: number = FOLLOW_THRESHOLD_PX,
): boolean {
  return scrollHeight - scrollTop - clientHeight <= threshold
}

/** 视口窗口切片（纯函数）：[start, end) 含 overscan，收拢到 [0, total]。 */
export function visibleRange(
  scrollTop: number,
  clientHeight: number,
  total: number,
  rowHeight: number = ROW_HEIGHT_PX,
  overscan: number = OVERSCAN_ROWS,
): { start: number; end: number } {
  if (total <= 0) return { start: 0, end: 0 }
  const start = Math.max(0, Math.floor(scrollTop / rowHeight) - overscan)
  const end = Math.min(total, start + Math.ceil(clientHeight / rowHeight) + overscan * 2)
  return { start, end }
}

/** 距最后一条事件超过阈值（且 live）→ 「长时间无输出」。 */
export function isIdle(
  lastEventTsMs: number | undefined,
  nowMs: number,
  threshold: number = IDLE_THRESHOLD_MS,
): boolean {
  if (lastEventTsMs === undefined) return false
  return nowMs - lastEventTsMs > threshold
}

/** 渲染行：虚拟化的原子单位，恰好一条单行文本（行高 22px 不变量）。 */
export interface TranscriptLine {
  key: string
  kind: 'start' | 'exit' | 'out' | 'err' | 'think' | 'tool' | 'toolres' | 'usage'
  /** 仅 exit 行：status === 'failed' 时加 t-failed 红显 */
  failed?: boolean
  /** 仅 toolres 行：isError 红显 */
  toolError?: boolean
  text: string
}

const ANSI_PATTERN = /\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))/g
// 裸 C0 控制符（含垂直制表等）一并剥除——pre 下不可见且扰动行高
const CONTROL_PATTERN = /[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g

/** 剥 ANSI 转义（CSI/OSC 序列）与残留控制符；\r\n/\n/\r 留给 splitLines。 */
export function stripAnsi(text: string): string {
  return text.replace(ANSI_PATTERN, '').replace(CONTROL_PATTERN, '')
}

/** 按 \r\n / \n / \r 切行。CSS pre 会把 \r 渲染成换行——在数据层先展开，
 *  终端式 \r 覆盖重写按行展开呈现（不追踪最终帧，v1 语义）。 */
export function splitLines(text: string): string[] {
  return text.split(/\r\n|\n|\r/)
}

/** 事件流 → 渲染行流（纯函数）。多行来源：session_start 携带的完整
 *  DISPATCH_PROMPT（argv 内嵌 \n）、子进程输出中的 \r 进度重写。 */
export function flattenEvents(events: TranscriptEvent[]): TranscriptLine[] {
  const lines: TranscriptLine[] = []
  for (const event of events) {
    let kind: TranscriptLine['kind']
    let failed = false
    let toolError = false
    let text: string
    if (event.kind === 'session_start') {
      kind = 'start'
      text = `▶ ${event.program} ${event.args.join(' ')} · pid ${event.pid} · ${event.cwd}`
    } else if (event.kind === 'exit') {
      kind = 'exit'
      failed = event.status === 'failed'
      text = `■ ${event.status}${event.code !== null ? ` · code ${event.code}` : ''}${event.killReason ? ` · ${event.killReason}` : ''} · ${(event.durationMs / 1000).toFixed(1)}s`
    } else if (event.kind === 'assistant_text') {
      kind = 'out'
      text = event.text
    } else if (event.kind === 'assistant_thinking') {
      kind = 'think'
      text = event.text
    } else if (event.kind === 'tool_call') {
      kind = 'tool'
      const input = JSON.stringify(event.input ?? {})
      text = `⚙ ${event.name} ${input.length > 200 ? `${input.slice(0, 200)}…` : input}`
    } else if (event.kind === 'tool_result') {
      kind = 'toolres'
      toolError = event.isError
      text = `↳ ${event.summary}`
    } else if (event.kind === 'usage') {
      kind = 'usage'
      const tokens = event.tokens != null ? ` · ${event.tokens.toLocaleString()} tok` : ''
      const cost = event.cost != null ? ` · $${event.cost.toFixed(4)}` : ''
      text = `⚡${tokens}${cost}`
    } else if (event.kind === 'error') {
      kind = 'err'
      text = `⚠ ${event.message}`
    } else {
      kind = event.stream === 'stderr' ? 'err' : 'out'
      text = event.text
    }
    for (const [i, line] of splitLines(stripAnsi(text)).entries()) {
      // toolError 仅 toolres 行携带（深比较断言面保持最小）
      lines.push({
        key: `${event.kind}-${event.seq}-${i}`,
        kind,
        failed,
        ...(kind === 'toolres' ? { toolError } : {}),
        text: line,
      })
    }
  }
  return lines
}

/** 普通视图的消息分组（U-VIEW）：统一事件 → 聊天式消息块。
 * tool_call 与其后的 tool_result 配对一个条目；thinking 块独立可折叠；
 * start/exit 保持单行。纯函数供测试。 */
export type MessageGroup =
  | { kind: 'start'; text: string }
  | { kind: 'exit'; text: string; failed: boolean }
  | { kind: 'thinking'; text: string }
  | { kind: 'text'; text: string }
  | { kind: 'tool'; name: string; input: string; summary: string; isError: boolean; pendingResult?: boolean }
  | { kind: 'error'; text: string }
  | { kind: 'raw'; stream: string; text: string }


/** 普通视图的轮次结构（U-VIEW2，对齐 dsh Turn 语义）：一轮 = 一段过程
 *（思考/工具/原始输出）+ 轮末回答（assistant_text）+ 可选用量。完成的轮
 * 折叠为摘要头（已思考 · N 次工具调用 · 用时——数据有就显示）；运行中
 * 轮平铺。session_start/exit 是轮外的顶层分隔行。纯函数供测试。 */
export interface Turn {
  /** 轮内过程与回答条目（MessageGroup 同构，含 text 终答） */
  entries: MessageGroup[]
  /** 过程行计数（thinking + tool） */
  thinkingCount: number
  toolCount: number
  /** 首尾事件 ts 差（毫秒；事件缺 ts 时为 null——有就显示没有就空着） */
  durationMs: number | null
  /** 轮末用量（事件携带时） */
  usage: { tokens?: number | null; cost?: number | null } | null
  /** 已收到 assistant_text（轮完成）——之后的事件开新轮 */
  answered: boolean
}

export function groupTurns(events: TranscriptEvent[]): Turn[] {
  const turns: Turn[] = []
  let current: Turn = { entries: [], thinkingCount: 0, toolCount: 0, durationMs: null, usage: null, answered: false }
  let firstTs: number | null = null
  let lastTs: number | null = null
  const sealTurn = () => {
    if (current.entries.length === 0) {
      return // 空轮不产出
    }
    current.durationMs =
      firstTs !== null && lastTs !== null && lastTs >= firstTs ? lastTs - firstTs : null
    turns.push(current)
    current = { entries: [], thinkingCount: 0, toolCount: 0, durationMs: null, usage: null, answered: false }
    firstTs = null
    lastTs = null
  }
  for (const event of events) {
    // 已答轮的延后封口（dsh 时序：usage 在回答之后到达，须归入本轮）——
    // 新过程/新回答事件才真正开新轮。先于 ts 记账：新轮首事件的时间戳
    // 不得污染上一轮时长。
    if (
      current.answered &&
      (event.kind === 'assistant_thinking' ||
        event.kind === 'tool_call' ||
        (event.kind === 'assistant_text' && current.entries.some(e => e.kind === 'text')))
    ) {
      sealTurn()
    }
    const ts = Number(event.ts)
    if (Number.isFinite(ts) && ts > 0) {
      if (firstTs === null) firstTs = ts
      lastTs = ts
    }
    // start/exit 是顶层分隔：封当前轮后单独成块（不进 Turn）
    if (event.kind === 'session_start' || event.kind === 'exit') {
      sealTurn()
      current.entries.push(
        event.kind === 'session_start'
          ? { kind: 'start', text: `▶ ${event.program} ${event.args.join(' ')} · pid ${event.pid}` }
          : {
              kind: 'exit',
              text: `■ ${event.status}${event.code !== null ? ` · code ${event.code}` : ''}${event.killReason ? ` · ${event.killReason}` : ''} · ${(event.durationMs / 1000).toFixed(1)}s`,
              failed: event.status === 'failed',
            },
      )
      current.answered = true // 分隔块独立成「轮」展示（不折叠）
      sealTurn()
      continue
    }
    switch (event.kind) {
      case 'assistant_text':
        current.entries.push({ kind: 'text', text: event.text })
        current.answered = true
        break
      case 'assistant_thinking':
        current.thinkingCount += 1
        current.entries.push({ kind: 'thinking', text: event.text })
        break
      case 'tool_call': {
        current.toolCount += 1
        const input = JSON.stringify(event.input ?? {})
        current.entries.push({
          kind: 'tool',
          name: event.name,
          input,
          summary: '',
          isError: false,
          pendingResult: true,
        })
        break
      }
      case 'tool_result': {
        // 配对到本轮最后一个待结果工具；配不上则独立 raw 呈现
        const pending = [...current.entries].reverse().find(
          g => g.kind === 'tool' && g.pendingResult,
        ) as { kind: 'tool'; pendingResult?: boolean } | undefined
        if (pending) {
          const tool = pending as Extract<MessageGroup, { kind: 'tool' }>
          tool.summary = event.summary
          tool.isError = event.isError
          tool.pendingResult = false
        } else {
          current.entries.push({ kind: 'raw', stream: 'stdout', text: `↳ ${event.summary}` })
        }
        break
      }
      case 'usage':
        current.usage = { tokens: event.tokens ?? null, cost: event.cost ?? null }
        break
      case 'error':
        current.entries.push({ kind: 'error', text: `⚠ ${event.message}` })
        break
      default: {
        // raw 行：JSON 形状（旧数据/未解析流）不进普通视图——Raw 视图
        // 才是它的归属地；纯文本（echo 输出等）正常显示
        const line = (event as { text: string }).text ?? ''
        if (!line.trimStart().startsWith('{')) {
          current.entries.push({
            kind: 'raw',
            stream: (event as { stream: string }).stream ?? 'stdout',
            text: line,
          })
        }
        break
      }
    }
  }
  sealTurn()
  return turns
}

interface Props {
  agent: AgentRecord | null
  events: TranscriptEvent[]
  /** 本 exec 已认领项数（两段式确认文案的 N；未知为 null） */
  claimed?: number | null
  onStopAgent?: () => void
  /** U10：空闲 agent 一键续跑（Agents 面板入口；live 时不渲染） */
  onResumeAgent?: () => void
  /** U14：该 agent 的 run 中待人工确认（信任门）的 item 数——流尾提示指路 */
  awaitingCount?: number | null
  /** F-DEL：删除 agent（继续执行右侧红色按钮；确认弹窗由 AgentsPanel 呈现） */
  onDeleteAgent?: () => void
}

export function TranscriptView({
  agent,
  events,
  claimed = null,
  onStopAgent,
  onResumeAgent,
  awaitingCount = null,
  onDeleteAgent,
}: Props) {
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const [follow, setFollow] = useState(true)
  const [unseen, setUnseen] = useState(0)
  const [range, setRange] = useState({ start: 0, end: 0 })
  /** U-VIEW：普通（消息分组）/ Raw（行式）双视图；默认普通 */
  const [viewMode, setViewMode] = useState<'normal' | 'raw'>('normal')
  const followRef = useRef(true)
  const lastSeenRef = useRef(0)
  // idle 求值用的时钟 tick：事件静默时不发生重渲染，Date.now() 只在渲染期
  // 求值会让「长时间无输出」在 60s 静默场景永不出现——live 期间用 interval
  // 定期 bump（cleanup 与 live 翻转时清理）。
  const [nowTick, setNowTick] = useState(() => Date.now())
  const live = agent !== null && agent.attachment === 'live' && (agent.status === 'running' || agent.status === 'starting')
  useEffect(() => {
    if (!live) return
    const timer = setInterval(() => setNowTick(Date.now()), IDLE_RECHECK_MS)
    return () => clearInterval(timer)
  }, [live])

  // events 是 store 尾窗的稳定数组引用（原地追加）——用 length 感知新事件；
  // 身份变化（切换 exec）由父组件按 execId 换 key（整体重挂载）解决。
  // lines 随 length 变化重展开（数组引用不变，dep 必须含 length）。
  const lines = useMemo(() => flattenEvents(events), [events, events.length])
  useEffect(() => {
    const el = scrollRef.current
    if (followRef.current) {
      lastSeenRef.current = lines.length
      setUnseen(0)
      // 直接赋值 scrollTop（不用 scrollTo：jsdom 无实现，测试以实例属性覆写验证）
      if (el) el.scrollTop = el.scrollHeight
    } else {
      setUnseen(lines.length - lastSeenRef.current)
    }
    if (el) setRange(visibleRange(el.scrollTop, el.clientHeight, lines.length))
    // viewMode 参与：切回 Raw 时 range 需重算（否则初始 {0,0} 切片为空）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events, events.length, viewMode])

  const handleScroll = () => {
    const el = scrollRef.current
    if (!el) return
    const nowFollow = shouldFollow(el.scrollTop, el.scrollHeight, el.clientHeight)
    followRef.current = nowFollow
    setFollow(nowFollow)
    if (nowFollow) {
      lastSeenRef.current = lines.length
      setUnseen(0)
    }
    setRange(visibleRange(el.scrollTop, el.clientHeight, lines.length))
  }

  const jumpToLatest = () => {
    const el = scrollRef.current
    followRef.current = true
    setFollow(true)
    lastSeenRef.current = lines.length
    setUnseen(0)
    if (el) {
      el.scrollTop = el.scrollHeight
      setRange(visibleRange(el.scrollTop, el.clientHeight, lines.length))
    }
  }

  if (!agent) {
    return <p className="empty-hint transcript-hint">从左侧选择一个 Agent 查看消息流</p>
  }

  const lastTsMs = events.length > 0 ? Number(events[events.length - 1].ts) : undefined
  const idle = live && isIdle(lastTsMs, nowTick)
  const slice = lines.slice(range.start, range.end)

  return (
    <div className="transcript-view">
      <div className="t-header">
        <span className="agent-name">{agentDisplayName(agent)}</span>
        <span className={`chip agent-status st-${agent.status}`}>{agent.status}</span>
        <button
          className={`chip${viewMode === 'normal' ? ' active' : ''}`}
          title="按消息类型分组呈现（思考/工具块折叠，对齐 dsh 交互）"
          onClick={() => setViewMode('normal')}
        >
          普通
        </button>
        <button
          className={`chip${viewMode === 'raw' ? ' active' : ''}`}
          title="原始行式视图（逐行，含未解析输出）"
          onClick={() => setViewMode('raw')}
        >
          Raw
        </button>
        <span className="row-spacer" />
        {/* F-EXEC-5：orphan（不受控、pid 存活）不可续跑——append 会把
            attachment 覆盖回 Live 丢失追踪；指路「请先强制终止」（入口在
            左侧列表行的「强制终止」按钮）。 */}
        {!live && agent.attachment === 'orphan' && (
          <span
            className="orphan-badge"
            title="执行进程已丢失、harness 仍在运行：请先在列表中强制终止该 agent，再重新派发"
          >
            续跑不可用·请先强制终止
          </span>
        )}
        {!live && onResumeAgent && agent.attachment !== 'orphan' && (
          <button
            className="chip"
            onClick={onResumeAgent}
            title={
              agent.lastSessionId
                ? `续跑上次会话（${agent.lastSessionId.slice(0, 8)}…）——保留该 agent 的模型上下文`
                : '该 Agent 无历史会话——将新开会话执行其关联 ticket'
            }
          >
            继续执行
          </button>
        )}
        {!live && onDeleteAgent && (
          <button
            className="chip danger-chip"
            title="删除该 agent（不可恢复）"
            onClick={onDeleteAgent}
          >
            删除
          </button>
        )}
        {live && onStopAgent && <ConfirmStopButton claimed={claimed} onConfirm={onStopAgent} />}
      </div>
      {events.length === 0 ? (
        <p className="empty-hint transcript-hint">
          {live
            ? '尚无输出——等待 agent 产生事件…'
            : '该执行已结束，无输出记录（历史尾部已尝试回载）'}
        </p>
      ) : viewMode === 'normal' ? (
        <NormalTranscriptView events={events} live={live} />
      ) : (
        <div className="t-scroll-wrap">
          <div className="t-scroll" ref={scrollRef} onScroll={handleScroll}>
            <div className="t-canvas" style={{ height: lines.length * ROW_HEIGHT_PX }}>
              {slice.map((line, index) => (
                <TranscriptRow
                  key={line.key}
                  line={line}
                  top={(range.start + index) * ROW_HEIGHT_PX}
                />
              ))}
            </div>
          </div>
          {!follow && (
            <button className="follow-pill" onClick={jumpToLatest}>
              回到最新 ({unseen})
            </button>
          )}
        </div>
      )}
      {idle && <p className="idle-hint">长时间无输出——agent 可能仍在等待外部操作</p>}
      {(awaitingCount ?? 0) > 0 && (
        // U14：信任门提示——agent 的完成主张待人工裁决（Runs 面板处理）
        <p className="idle-hint trust-gate-hint">
          ⚠ {awaitingCount} 项完成待人工确认（agent 绕过了 run 纪律的完成上报）——在下方弹窗中「确认 / 打回」
        </p>
      )}
    </div>
  )
}

/** 普通视图（U-VIEW）：消息分组呈现。不做行级虚拟化——尾窗事件有界
 *（2000 条），分组后 DOM 规模可控；折叠交互对齐 dsh（thinking 与工具
 * 执行默认折叠、点击展开）。 */
function NormalTranscriptView({ events, live }: { events: TranscriptEvent[]; live: boolean }) {
  // events 是 store 尾窗的稳定数组引用（原地追加）——memo 依赖必须含
  // length 才能感知实时事件（raw 视图 effect 同款；首版漏了 → 运行中
  // 永不更新、结束后也不显示，实测抓到）
  const turns = useMemo(() => groupTurns(events), [events, events.length])
  const [expanded, setExpanded] = useState<Record<string, boolean>>({})
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const stickRef = useRef(true)
  useEffect(() => {
    const el = scrollRef.current
    if (el && stickRef.current) el.scrollTop = el.scrollHeight
  }, [turns])
  return (
    <div className="t-scroll-wrap">
      <div
        className="t-scroll normal-scroll"
        ref={scrollRef}
        onScroll={() => {
          const el = scrollRef.current
          if (!el) return
          stickRef.current = shouldFollow(el.scrollTop, el.scrollHeight, el.clientHeight)
        }}
      >
        <div className="msg-list">
          {turns.map((turn, i) => {
            const first = turn.entries[0]
            const isSeparator =
              turn.entries.length === 1 && (first.kind === 'start' || first.kind === 'exit')
            if (isSeparator) {
              const entry = first as Extract<MessageGroup, { kind: 'start' | 'exit' }>
              return entry.kind === 'start' ? (
                <div key={i} className="msg-row msg-start">
                  <span className="msg-icon"><IconPlay /></span>
                  <span className="msg-text-body">{entry.text}</span>
                </div>
              ) : (
                <div key={i} className={`msg-row msg-exit${entry.failed ? ' msg-failed' : ''}`}>
                  <span className="msg-icon">{entry.failed ? <IconWarn /> : <IconStopSquare />}</span>
                  <span className="msg-text-body">{entry.text}</span>
                </div>
              )
            }
            const textEntries = turn.entries.filter(e => e.kind === 'text')
            const open = expanded[String(i)] === true
            const running = live && !turn.answered && i === turns.length - 1
            return (
              <div key={i} className="msg-turn">
                {/* 轮头摘要：icon 前置 + 颜色（无竖线） */}
                {turn.answered ? (
                  <button className="msg-fold turn-head" onClick={() => setExpanded(s => ({ ...s, [String(i)]: !open }))} aria-expanded={open}>
                    <span className="msg-icon"><IconDoc /></span>
                    <span className="msg-text-body">
                      {turn.thinkingCount > 0 ? `思考 · ` : ''}
                      {turn.toolCount > 0 ? `${turn.toolCount} 次工具调用` : ''}
                      {turn.thinkingCount === 0 && turn.toolCount === 0 ? '处理' : ''}
                      {turn.durationMs != null ? ` · ${(turn.durationMs / 1000).toFixed(1)}s` : ''}
                      {open ? ' ▾' : ' ▸'}
                    </span>
                  </button>
                ) : null}
                {/* 展开后的过程明细：icon + 左侧细竖线包裹（嵌套层级） */}
                {(open || running || !turn.answered) && turn.entries.some(e => e.kind !== 'text') ? (
                  <div className="turn-detail">
                    {turn.entries.map((group, j) => {
                      const entryOpen = expanded[`${i}-${j}`] === true
                      const toggle = () => setExpanded(s => ({ ...s, [`${i}-${j}`]: !entryOpen }))
                      switch (group.kind) {
                        case 'thinking':
                          return (
                            <div key={j} className="msg-block msg-thinking">
                              <button className="msg-fold" onClick={toggle} aria-expanded={entryOpen}>
                                <span className="msg-icon"><IconThink /></span>
                                <span className="msg-text-body">思考 {entryOpen ? '▾' : '▸'}</span>
                              </button>
                              {entryOpen ? (
                                <pre className="msg-body">{group.text}</pre>
                              ) : (
                                <div className="msg-summary">{firstLine(group.text)}</div>
                              )}
                            </div>
                          )
                        case 'tool':
                          return (
                            <div key={j} className={`msg-block msg-tool${group.isError ? ' msg-tool-error' : ''}`}>
                              <button className="msg-fold" onClick={toggle} aria-expanded={entryOpen}>
                                <span className="msg-icon"><IconTool /></span>
                                <span className="msg-text-body">
                                  {group.name}
                                  {group.summary ? ` · ${firstLine(group.summary)}` : group.pendingResult ? ' · …' : ''} {entryOpen ? '▾' : '▸'}
                                </span>
                              </button>
                              {entryOpen ? (
                                <>
                                  <pre className="msg-body">{prettyJson(group.input)}</pre>
                                  {group.summary ? (
                                    <pre className={`msg-body${group.isError ? ' msg-body-error' : ''}`}>
                                      {group.summary.length > 600 ? `${group.summary.slice(0, 600)}\n…（截断，完整内容见 Raw 视图 / raw.log）` : group.summary}
                                    </pre>
                                  ) : null}
                                </>
                              ) : null}
                            </div>
                          )
                        case 'error':
                          return (
                            <div key={j} className="msg-row msg-error">
                              <span className="msg-icon"><IconWarn /></span>
                              <span className="msg-text-body">{group.text}</span>
                            </div>
                          )
                        case 'raw':
                          return (
                            <div key={j} className={`msg-row${group.stream === 'stderr' ? ' msg-stderr' : ''}`}>
                              <span className="msg-icon"><IconDoc /></span>
                              <span className="msg-text-body">{group.text}</span>
                            </div>
                          )
                        default:
                          return null
                      }
                    })}
                  </div>
                ) : null}
                {/* 轮末回答：markdown 正文（icon 前置 💬） */}
                {textEntries.map((group, j) =>
                  group.kind === 'text' ? (
                    <div key={`t${j}`} className="msg-row msg-text">
                      <span className="msg-icon"><IconChat /></span>
                      <div className="msg-text-body md-prose">
                        <MarkdownText text={group.text} />
                      </div>
                    </div>
                  ) : null,
                )}
                {/* 本轮用量（icon ⚡ + 颜色，数据有就显示） */}
                {turn.usage ? (
                  <div className="msg-row msg-usage">
                    <span className="msg-icon"><IconBolt /></span>
                    <span className="msg-text-body">
                      本轮用量
                      {turn.usage.tokens != null ? ` · ${turn.usage.tokens.toLocaleString()} tok` : ''}
                      {turn.usage.cost != null ? ` · $${turn.usage.cost.toFixed(4)}` : ''}
                    </span>
                  </div>
                ) : null}
              </div>
            )
          })}
          {live && <div className="msg-row msg-running">● 运行中…</div>}
        </div>
      </div>
    </div>
  )
}

function prettyJson(input: string): string {
  try {
    return JSON.stringify(JSON.parse(input), null, 2)
  } catch {
    return input
  }
}

function firstLine(text: string): string {
  const line = text.split('\n')[0] ?? ''
  return line.length > 80 ? `${line.slice(0, 80)}…` : line
}

function TranscriptRow({ line, top }: { line: TranscriptLine; top: number }) {
  const style = { top, left: 0, right: 0 } as const
  if (line.kind === 'exit') {
    return (
      <div className={`t-row t-exit${line.failed ? ' t-failed' : ''}`} style={style}>
        {line.text}
      </div>
    )
  }
  const cls =
    line.kind === 'start'
      ? 't-row t-start'
      : line.kind === 'err'
        ? 't-row t-stderr'
        : line.kind === 'think'
          ? 't-row t-think'
          : line.kind === 'tool'
            ? 't-row t-tool'
            : line.kind === 'toolres'
              ? `t-row t-toolres${line.toolError ? ' t-toolres-error' : ''}`
              : line.kind === 'usage'
                ? 't-row t-usage'
                : 't-row'
  return (
    <div className={cls} style={style}>
      {line.text}
    </div>
  )
}
