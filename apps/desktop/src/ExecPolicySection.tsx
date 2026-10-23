/**
 * A8 设置页「派发策略」（设计 §3.6 / KD-E5）：exec/policy.json 的界面编辑入口
 * （同一用户的进程也能直接改这个文件——见计划「威胁模型」）。只约束 CLI / MCP
 * 发起的派发；桌面端派发不受影响（Q-4）。
 */
import { useCallback, useEffect, useState } from 'react'
import { execPolicyGet, execPolicySet } from './bridge'
import type { ExecPolicy, ExecPolicyView } from './types'

const HARNESS_IDS = ['pi', 'omp', 'opencode'] as const

type FieldError = { field: 'maxConcurrent' | 'maxDurationSec'; text: string }

function validate(p: ExecPolicy): FieldError | null {
  if (!Number.isInteger(p.maxConcurrent) || p.maxConcurrent < 1 || p.maxConcurrent > 32) {
    return { field: 'maxConcurrent', text: '最大并发须在 1–32 之间' }
  }
  if (!Number.isInteger(p.maxDurationSec) || p.maxDurationSec < 60) {
    return { field: 'maxDurationSec', text: '最长时长不得小于 60 秒' }
  }
  return null
}

/** 空输入保持空串让校验报错，而不是被 Number('') 变成 0 */
const numberOrNaN = (raw: string) => (raw.trim() === '' ? Number.NaN : Number(raw))

export function ExecPolicySection() {
  const [view, setView] = useState<ExecPolicyView | null>(null)
  const [draft, setDraft] = useState<ExecPolicy | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [fieldError, setFieldError] = useState<FieldError | null>(null)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const [saving, setSaving] = useState(false)

  const load = useCallback(() => {
    setLoadError(null)
    void execPolicyGet()
      .then(v => {
        setView(v)
        // 文件无效时预填文档默认值（不是「全部拒绝」）——保存即修复成默认策略
        setDraft(v.policy ?? v.defaults)
      })
      .catch(error => setLoadError(String(error)))
  }, [])

  useEffect(load, [load])

  if (loadError) {
    return (
      <div role="alert" className="error">
        读取派发策略失败：{loadError}
        <button className="chip" onClick={load}>重试</button>
      </div>
    )
  }
  if (!view || !draft) return <p className="muted">读取派发策略…</p>

  const policy = draft
  const update = (patch: Partial<ExecPolicy>) => {
    setFieldError(null)
    setSaveError(null)
    setSaved(false)
    setDraft({ ...policy, ...patch })
  }
  const save = async () => {
    const problem = validate(policy)
    if (problem) {
      setFieldError(problem)
      return
    }
    setSaving(true)
    try {
      const next = await execPolicySet(policy)
      setView(next)
      setDraft(next.policy ?? policy)
      setSaved(true)
    } catch (error) {
      setSaveError(String(error))
    } finally {
      setSaving(false)
    }
  }
  const invalid = (field: FieldError['field']) => fieldError?.field === field

  return (
    <section className="exec-policy">
      <h3>派发策略</h3>
      <p className="muted">
        约束主控 agent（MCP）和命令行（CLI）发起的派发；桌面端派发不受影响。保存后只影响之后的派发，不会停止正在运行的执行。文件：<code>{view.path}</code>
        {/* 注：本句避免出现「默认值」三字——与「恢复默认值」按钮共用 /默认值/ 查询会撞多匹配 */}
        {view.source === 'default' && '（当前为出厂默认，尚未保存过）'}
      </p>
      <p className="warning">
        经 CLI / MCP 派发的 harness 一律无人值守运行（pi <code>--auto</code>、omp <code>--approval-mode=yolo</code> 等），没有桌面端派发弹窗那一步确认。
      </p>
      {view.error && (
        <p className="error" role="alert">
          策略文件无效：{view.error}。在修复前 CLI 和 MCP 派发已全部拒绝；下面已填入默认值，保存将用它覆盖文件。
        </p>
      )}
      <label>
        <input type="checkbox" checked={policy.allowFromCli} onChange={e => update({ allowFromCli: e.target.checked })} />
        允许经 CLI 派发
      </label>
      <p className="muted">也覆盖所有能执行 shell 的 agent（例如 Claude Code 的 Bash 工具）——只关 MCP 派发拦不住这类主控。</p>
      <label>
        <input type="checkbox" checked={policy.allowFromMcp} onChange={e => update({ allowFromMcp: e.target.checked })} />
        允许经 MCP 派发
      </label>
      <p className="muted">开启后，接入 omt mcp 的主控 agent 可在下面的并发与时长上限内启动 harness 进程。</p>
      <fieldset>
        <legend>允许的 harness</legend>
        {HARNESS_IDS.map(id => (
          <label key={id}>
            <input
              type="checkbox"
              checked={policy.harnesses.includes(id)}
              onChange={e => update({
                harnesses: e.target.checked ? [...policy.harnesses, id] : policy.harnesses.filter(h => h !== id),
              })}
            />
            {id}
          </label>
        ))}
        {policy.harnesses.length === 0 && (
          <p className="warning">一个 harness 都没选——所有 CLI / MCP 派发都会被拒绝。</p>
        )}
      </fieldset>
      <label>
        最大并发
        <input type="number" min={1} max={32} value={Number.isNaN(policy.maxConcurrent) ? '' : policy.maxConcurrent}
          aria-invalid={invalid('maxConcurrent')}
          aria-describedby={invalid('maxConcurrent') ? 'exec-policy-error' : undefined}
          onChange={e => update({ maxConcurrent: numberOrNaN(e.target.value) })} />
      </label>
      <label>
        最长时长（秒）
        <input type="number" min={60} value={Number.isNaN(policy.maxDurationSec) ? '' : policy.maxDurationSec}
          aria-invalid={invalid('maxDurationSec')}
          aria-describedby={invalid('maxDurationSec') ? 'exec-policy-error' : undefined}
          onChange={e => update({ maxDurationSec: numberOrNaN(e.target.value) })} />
      </label>
      <button onClick={() => void save()} disabled={saving}>{saving ? '保存中…' : '保存'}</button>
      <button className="chip" onClick={() => update(view.defaults)} disabled={saving}>恢复默认值</button>
      {(fieldError || saveError) && (
        <p id="exec-policy-error" className="error" role="alert">{fieldError?.text ?? saveError}</p>
      )}
      <p role="status" aria-live="polite">{saved ? '已保存' : ''}</p>
    </section>
  )
}
