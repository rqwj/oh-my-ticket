/**
 * U5 agents UI 叶模块：跨面板共享的最小展示原语——`agentDisplayName`
 * 展示名与 `ConfirmStopButton` 两段式终止控件。独立成叶（只依赖 react 与
 * types，不 import 任何面板模块）：TranscriptView / TreePanel /
 * ExecutionDialog 从这里取符号，避免此前「面板模块当工具枢纽」造成的
 * TranscriptView ↔ AgentsPanel 循环导入。
 */
import { useEffect, useState } from 'react'
import type { AgentRecord } from './types'

const CONFIRM_RESET_MS = 3000

/** 展示名：harness 给出的 title 优先，回退记录 displayName（P0 即 Agent N）。 */
export function agentDisplayName(agent: AgentRecord): string {
  return agent.title ?? agent.displayName
}

/**
 * 两段式终止控件（原 §九-8）：首击变行内「确认终止：将释放 N 个已认领项」
 * （N 未知时省略计数），3s 无操作还原，二击执行。
 */
export function ConfirmStopButton({
  claimed,
  onConfirm,
}: {
  claimed: number | null
  onConfirm: () => void
}) {
  const [arming, setArming] = useState(false)
  useEffect(() => {
    if (!arming) return
    const timer = setTimeout(() => setArming(false), CONFIRM_RESET_MS)
    return () => clearTimeout(timer)
  }, [arming])
  if (!arming) {
    return (
      <button
        className="chip agent-stop-btn"
        onClick={event => {
          event.stopPropagation()
          setArming(true)
        }}
      >
        终止
      </button>
    )
  }
  return (
    <span className="confirm-stop" onClick={event => event.stopPropagation()}>
      确认终止{claimed !== null ? `：将释放 ${claimed} 个已认领项` : ''}
      <button
        className="chip active"
        onClick={event => {
          event.stopPropagation()
          onConfirm()
        }}
      >
        确认终止
      </button>
      <button
        className="chip"
        onClick={event => {
          event.stopPropagation()
          setArming(false)
        }}
      >
        取消
      </button>
    </span>
  )
}

/**
 * review F7/F14（P-10）：「停止全部」两段式确认（误点保护不弱于单个终止），
 * 进行中禁用；确认文案带运行数与已认领项总数。
 */
export function ConfirmStopAllButton({
  running,
  claimed,
  busy,
  onConfirm,
}: {
  running: number
  claimed: number | null
  busy: boolean
  onConfirm: () => void
}) {
  const [arming, setArming] = useState(false)
  useEffect(() => {
    if (!arming) return
    const timer = setTimeout(() => setArming(false), CONFIRM_RESET_MS)
    return () => clearTimeout(timer)
  }, [arming])
  if (busy) {
    return (
      <button type="button" className="chip agents-stop-all" disabled>
        停止中…
      </button>
    )
  }
  if (!arming) {
    return (
      <button
        type="button"
        className="chip agents-stop-all"
        title="只停止当前 workspace 中可控的 agent；不受控（orphan）的需单独强制终止"
        onClick={() => setArming(true)}
      >
        停止全部
      </button>
    )
  }
  return (
    <span className="confirm-stop agents-stop-all">
      确认停止 {running} 个 agent{claimed !== null ? `，将释放 ${claimed} 个已认领项` : ''}
      <button
        type="button"
        className="chip active"
        onClick={() => {
          setArming(false)
          onConfirm()
        }}
      >
        确认
      </button>
      <button type="button" className="chip" onClick={() => setArming(false)}>
        取消
      </button>
    </span>
  )
}
