/**
 * U5 (R-E): TranscriptView —— 选中 agent 最新 exec 的尾窗渲染。
 *
 * 虚拟化为手写实现（repo 无滚动依赖，不新增 npm 包）：固定行高 + 视口
 * 窗口切片（visibleRange 纯函数），滚动/事件到达时重算。行高固定换取
 * 简单可靠的切片语义，raw 行不做 Markdown 渲染（可变高度会破坏窗口
 * 坐标）；stdout/stderr 用样式区分，session_start/exit 结构化呈现。
 *
 * 自动跟随（原 §九-13）：视口近底（FOLLOW_THRESHOLD_PX）才跟随新事件；
 * 上滚解除跟随并显示「回到最新 (N)」pill，点击回贴底恢复。live 且长时间
 * 无输出（IDLE_THRESHOLD_MS）显示「长时间无输出」提示（§3.2）。
 */
import { useEffect, useRef, useState } from 'react'
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
  useEffect(() => {
    const el = scrollRef.current
    if (followRef.current) {
      lastSeenRef.current = events.length
      setUnseen(0)
      // 直接赋值 scrollTop（不用 scrollTo：jsdom 无实现，测试以实例属性覆写验证）
      if (el) el.scrollTop = el.scrollHeight
    } else {
      setUnseen(events.length - lastSeenRef.current)
    }
    if (el) setRange(visibleRange(el.scrollTop, el.clientHeight, events.length))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events, events.length])

  const handleScroll = () => {
    const el = scrollRef.current
    if (!el) return
    const nowFollow = shouldFollow(el.scrollTop, el.scrollHeight, el.clientHeight)
    followRef.current = nowFollow
    setFollow(nowFollow)
    if (nowFollow) {
      lastSeenRef.current = events.length
      setUnseen(0)
    }
    setRange(visibleRange(el.scrollTop, el.clientHeight, events.length))
  }

  const jumpToLatest = () => {
    const el = scrollRef.current
    followRef.current = true
    setFollow(true)
    lastSeenRef.current = events.length
    setUnseen(0)
    if (el) {
      el.scrollTop = el.scrollHeight
      setRange(visibleRange(el.scrollTop, el.clientHeight, events.length))
    }
  }

  if (!agent) {
    return <p className="empty-hint transcript-hint">从左侧选择一个 Agent 查看消息流</p>
  }

  const lastTsMs = events.length > 0 ? Number(events[events.length - 1].ts) : undefined
  const idle = live && isIdle(lastTsMs, nowTick)
  const slice = events.slice(range.start, range.end)

  return (
    <div className="transcript-view">
      <div className="t-header">
        <span className="agent-name">{agentDisplayName(agent)}</span>
        <span className={`chip agent-status st-${agent.status}`}>{agent.status}</span>
        <span className="row-spacer" />
        {live && onStopAgent && <ConfirmStopButton claimed={claimed} onConfirm={onStopAgent} />}
      </div>
      {events.length === 0 ? (
        <p className="empty-hint transcript-hint">尚无输出——等待 agent 产生事件…</p>
      ) : (
        <div className="t-scroll-wrap">
          <div className="t-scroll" ref={scrollRef} onScroll={handleScroll}>
            <div className="t-canvas" style={{ height: events.length * ROW_HEIGHT_PX }}>
              {slice.map((event, index) => (
                <TranscriptRow
                  key={`${event.kind}-${event.seq}`}
                  event={event}
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

function TranscriptRow({ event, top }: { event: TranscriptEvent; top: number }) {
  const style = { top, left: 0, right: 0 } as const
  if (event.kind === 'session_start') {
    return (
      <div className="t-row t-start" style={style}>
        ▶ {event.program} {event.args.join(' ')} · pid {event.pid} · {event.cwd}
      </div>
    )
  }
  if (event.kind === 'exit') {
    return (
      <div className={`t-row t-exit${event.status === 'failed' ? ' t-failed' : ''}`} style={style}>
        ■ {event.status}
        {event.code !== null ? ` · code ${event.code}` : ''}
        {event.killReason ? ` · ${event.killReason}` : ''} · {(event.durationMs / 1000).toFixed(1)}s
      </div>
    )
  }
  return (
    <div className={`t-row${event.stream === 'stderr' ? ' t-stderr' : ''}`} style={style}>
      {event.text}
    </div>
  )
}
