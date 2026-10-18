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
  kind: 'start' | 'exit' | 'out' | 'err'
  /** 仅 exit 行：status === 'failed' 时加 t-failed 红显 */
  failed?: boolean
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
    let text: string
    if (event.kind === 'session_start') {
      kind = 'start'
      text = `▶ ${event.program} ${event.args.join(' ')} · pid ${event.pid} · ${event.cwd}`
    } else if (event.kind === 'exit') {
      kind = 'exit'
      failed = event.status === 'failed'
      text = `■ ${event.status}${event.code !== null ? ` · code ${event.code}` : ''}${event.killReason ? ` · ${event.killReason}` : ''} · ${(event.durationMs / 1000).toFixed(1)}s`
    } else {
      kind = event.stream === 'stderr' ? 'err' : 'out'
      text = event.text
    }
    for (const [i, line] of splitLines(stripAnsi(text)).entries()) {
      lines.push({ key: `${event.kind}-${event.seq}-${i}`, kind, failed, text: line })
    }
  }
  return lines
}

interface Props {
  agent: AgentRecord | null
  events: TranscriptEvent[]
  /** 本 exec 已认领项数（两段式确认文案的 N；未知为 null） */
  claimed?: number | null
  onStopAgent?: () => void
}

export function TranscriptView({ agent, events, claimed = null, onStopAgent }: Props) {
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const [follow, setFollow] = useState(true)
  const [unseen, setUnseen] = useState(0)
  const [range, setRange] = useState({ start: 0, end: 0 })
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events, events.length])

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
        <span className="row-spacer" />
        {live && onStopAgent && <ConfirmStopButton claimed={claimed} onConfirm={onStopAgent} />}
      </div>
      {events.length === 0 ? (
        <p className="empty-hint transcript-hint">
          {live
            ? '尚无输出——等待 agent 产生事件…'
            : '该执行已结束，无输出记录（历史尾部已尝试回载）'}
        </p>
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
    </div>
  )
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
  const cls = line.kind === 'start' ? 't-row t-start' : `t-row${line.kind === 'err' ? ' t-stderr' : ''}`
  return (
    <div className={cls} style={style}>
      {line.text}
    </div>
  )
}
