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
