/**
 * U10 runs panel: run list + item detail with claim/report confirmation
 * flows. Lease semantics are server-fenced — the UI presents fence
 * rejections (expired lease) as readable banners rather than hiding them.
 */
import { useState } from 'react'
import { omtCall } from './bridge'
import type { HomeInfo, RunDetail, RunSummary } from './types'

interface Props {
  home: HomeInfo
  runs: RunSummary[]
  fetchRun: (runId: string) => Promise<RunDetail>
  onChanged: () => void
}

export function RunsPanel({ home, runs, fetchRun, onChanged }: Props) {
  const [detail, setDetail] = useState<RunDetail | null>(null)
  const [banner, setBanner] = useState<string | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)

  const open = async (runId: string) => {
    try {
      setDetail(await fetchRun(runId))
      setBanner(null)
    } catch (error) {
      setBanner(String(error))
    }
  }

  const control = async (runId: string, action: string) => {
    try {
      await omtCall('run/control', { homeId: home.homeId, runId, action })
      setBanner(null)
      await open(runId)
      onChanged()
    } catch (error) {
      const text = String(error)
      setBanner(
        text.includes('lease') || text.includes('fence')
          ? `操作被 lease fence 拒绝（租约已过期或归属变更）— 请刷新后确认当前执行者。(${text})`
          : text,
      )
    } finally {
      setConfirming(null)
    }
  }

  /** U14 信任门确认：admin report done（item AwaitingConfirmation → Done）。 */
  const confirmItem = async (runId: string, nodeId: string) => {
    try {
      await omtCall('run/report', {
        homeId: home.homeId,
        runId,
        nodeId,
        outcome: 'done',
        note: '人工确认（信任门 awaiting_confirmation）',
      })
      setBanner(null)
      await open(runId)
      onChanged()
    } catch (error) {
      setBanner(String(error))
    }
  }

  /** U14 信任门打回：ticket 重开（node open）——daemon 联动把 item 转
   * Interrupted（等待重新派发认领），不认可 agent 的完成主张。 */
  const rejectItem = async (runId: string, nodeId: string) => {
    try {
      await omtCall('node/update', {
        homeId: home.homeId,
        nodeId,
        changes: { status: 'open' },
      })
      setBanner(null)
      await open(runId)
      onChanged()
    } catch (error) {
      setBanner(String(error))
    }
  }

  return (
    <div className="runs-panel">
      <h3>Runs</h3>
      {banner && <p className="error-banner">{banner}</p>}
      <div className="runs-list">
        {runs.map(run => (
          <button key={run.id} className={`run-row${detail?.run.id === run.id ? ' selected' : ''}`} onClick={() => void open(run.id)}>
            <span className="node-id">{run.id}</span>
            <span className="run-title">{run.title ?? ''}</span>
            {run.progress && (
              <span className="run-progress">{run.progress.done}/{run.progress.total}</span>
            )}
            <span className="chip">{run.status}</span>
          </button>
        ))}
        {runs.length === 0 && <p className="empty-hint">当前 home 无 run</p>}
      </div>
      {detail && (
        <div className="run-detail">
          <div className="run-actions">
            {['pause', 'resume', 'cancel'].map(action =>
              confirming === action ? (
                <span key={action} className="confirm-group">
                  确认 {action}？
                  <button className="chip active" onClick={() => void control(detail.run.id, action)}>是</button>
                  <button className="chip" onClick={() => setConfirming(null)}>否</button>
                </span>
              ) : (
                <button key={action} className="chip" onClick={() => setConfirming(action)}>
                  {action}
                </button>
              ),
            )}
          </div>
          <table className="items-table">
            <thead>
              <tr><th>节点</th><th>状态</th><th>执行者</th><th>尝试</th><th>错误</th></tr>
            </thead>
            <tbody>
              {detail.items.map(item => (
                <tr key={item.nodeId} className={item.stalled ? 'stalled-row' : ''}>
                  <td><span className="node-id">{item.nodeId}</span>{item.title ? ` ${item.title}` : ''}</td>
                  <td>
                    {item.state === 'awaiting_confirmation' ? (
                      // U14 信任门：agent 绕过 run/report 的完成主张，需人工
                      // 裁决——确认（report done）或打回（ticket 重开→interrupted）
                      <span className="trust-gate-cell">
                        <span className="chip trust-gate">待确认</span>
                        <button
                          className="chip active"
                          title="认可完成：item → done，ticket 保持 done"
                          onClick={() => void confirmItem(detail.run.id, item.nodeId)}
                        >
                          确认
                        </button>
                        <button
                          className="chip"
                          title="打回：ticket 重开 → item 转 interrupted，可重新派发"
                          onClick={() => void rejectItem(detail.run.id, item.nodeId)}
                        >
                          打回
                        </button>
                      </span>
                    ) : (
                      item.state
                    )}
                  </td>
                  <td>{item.executorActor ?? '—'}</td>
                  <td>{item.attempts}</td>
                  <td className="error-cell">{item.lastError ?? ''}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
