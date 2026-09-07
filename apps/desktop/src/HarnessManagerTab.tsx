/**
 * U7 (R-A)：设置页 Harnesses 完整管理面——左类型列表（检测状态 badge）+
 * 右详情（Enabled / CLI & Args / Env）。
 *
 * - Enabled 开关即时保存（harnessConfigSet 以完整草稿 + 翻转后的 enabled
 *   调用，返回的最新投影更新列表并回显探测结果）；
 * - binary 指定：Detected（路径不落盘，每次派发现场重探）/ Manual（路径
 *   输入 + 文件选择器；检测不到时必配）；保存时探测并回显版本，探测失败
 *   可保存但标「待验证」（state=pendingVerification）；
 * - Env KV 表：键值编辑 + 增删；前端镜像 Rust `validate_env`/`validate_config`
 *   （空键、含 '='、保留键 OMT_RUNTIME_DIR/OMT_HOME/PATH 精确匹配拒绝；
 *   值允许空；Manual 空路径拒绝）——给出比 invoke 更早的可读报错；
 * - 禁用语义呈现：禁用仅挡新派发——该 harness 有存活 Agent（starting/
 *   running）时详情区提示 N 与去向（Agents 面板）；计数来源 = tab 挂载时
 *   对各 open home 拉 agents_list 汇总（拉取失败按 0 计，不阻塞页面）；
 *   终止动作不做（引导去 Agents 面板）。
 * - extraArgs 采用一行一参数：空格分词会破坏含空格/引号的参数值。
 */
import { useEffect, useState } from 'react'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { agentsList } from './bridge'
import { harnessConfigSet, harnessList, type HarnessEntry, type HarnessUserConfigInput } from './store'
import type { HomeInfo } from './types'

/** 保留键镜像 Rust harness.rs 的 RESERVED_ENV_KEYS（精确匹配，不做大小写折叠）。 */
const RESERVED_ENV_KEYS = ['OMT_RUNTIME_DIR', 'OMT_HOME', 'PATH']

const STATE_TEXT: Record<HarnessEntry['state'], string> = {
  ready: '就绪',
  notDetected: '未检测到',
  pendingVerification: '待验证',
}

interface EnvRow {
  key: string
  value: string
}

/** 选中 provider 的编辑草稿（保存/开关共用；切换选中或保存回包后重置）。 */
interface HarnessDraft {
  enabled: boolean
  binaryKind: 'detected' | 'manual'
  manualPath: string
  /** 一行一个参数 */
  extraArgsText: string
  envRows: EnvRow[]
}

const draftOf = (entry: HarnessEntry): HarnessDraft => ({
  enabled: entry.enabled,
  binaryKind: entry.binarySource,
  manualPath: entry.manualPath ?? '',
  extraArgsText: (entry.extraArgs ?? []).join('\n'),
  envRows: Object.entries(entry.env ?? {}).map(([key, value]) => ({ key, value })),
})

/** 前端镜像 Rust validate_env（空键 / 含 '=' / 保留键；值允许空）。 */
function validateEnvRows(rows: EnvRow[]): string | null {
  for (const row of rows) {
    if (row.key === '') return '环境变量名不能为空'
    if (row.key.includes('=')) return `非法环境变量名：「${row.key}」（不能含 '='）`
    if (RESERVED_ENV_KEYS.includes(row.key)) return `环境变量 ${row.key} 为保留键（由 desktop 注入），拒绝保存`
  }
  return null
}

/** 草稿 → harness_config_set 入参。 */
function buildConfig(draft: HarnessDraft): HarnessUserConfigInput {
  return {
    enabled: draft.enabled,
    binary: draft.binaryKind === 'manual' ? { kind: 'manual', path: draft.manualPath.trim() } : { kind: 'detected' },
    extraArgs: draft.extraArgsText.split('\n').map(line => line.trim()).filter(line => line.length > 0),
    env: Object.fromEntries(draft.envRows.map(row => [row.key, row.value])),
  }
}

interface Props {
  /** open home 集合：挂载时逐个拉 agents_list 汇总存活 Agent 计数 */
  homes: HomeInfo[]
}

export function HarnessManagerTab({ homes }: Props) {
  const [entries, setEntries] = useState<HarnessEntry[] | null>(null)
  const [selectedId, setSelectedId] = useState('')
  const [draft, setDraft] = useState<HarnessDraft | null>(null)
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [savedNote, setSavedNote] = useState<string | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  /** 各 harness 的存活 Agent 数（starting/running；拉取失败的 home 按 0 计） */
  const [liveCounts, setLiveCounts] = useState<Record<string, number>>({})

  // 挂载 = 设置页切入 Harnesses tab：拉全量投影 + 各 open home 的存活计数。
  useEffect(() => {
    let cancelled = false
    void harnessList()
      .then(list => {
        if (cancelled) return
        setEntries(list)
        setSelectedId(prev => prev || list[0]?.id || '')
        // 详情草稿随首选项初始化（切换选中时由 selectHarness 重置）
        setDraft(prev => prev ?? (list[0] ? draftOf(list[0]) : null))
      })
      .catch(error => {
        if (!cancelled) setLoadError(String(error))
      })
    void Promise.allSettled(
      homes.map(async home => (await agentsList(home.homeId)).agents ?? []),
    ).then(outcomes => {
      if (cancelled) return
      const counts: Record<string, number> = {}
      for (const outcome of outcomes) {
        if (outcome.status !== 'fulfilled') continue // 拉取失败按 0 计，不阻塞页面
        for (const agent of outcome.value) {
          if (agent.status === 'starting' || agent.status === 'running') {
            counts[agent.harness] = (counts[agent.harness] ?? 0) + 1
          }
        }
      }
      setLiveCounts(counts)
    })
    return () => {
      cancelled = true
    }
    // tab 挂载时拉一次（§3.4 pull 契约）；homes 快照即本次计数口径
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const selectHarness = (id: string) => {
    setSelectedId(id)
    const entry = entries?.find(e => e.id === id)
    if (entry) setDraft(draftOf(entry))
    setSaveError(null)
    setSavedNote(null)
  }

  /** 保存一份完整配置：用返回的最新投影更新列表并重置草稿（回显探测结果）。 */
  const persist = async (harnessId: string, config: HarnessUserConfigInput) => {
    setSaving(true)
    try {
      const updated = await harnessConfigSet(harnessId, config)
      setEntries(list => (list ?? []).map(e => (e.id === updated.id ? updated : e)))
      setDraft(draftOf(updated))
      setSaveError(null)
      setSavedNote(
        updated.state === 'pendingVerification'
          ? '已保存。binary 探测失败，标记为「待验证」——派发时会再次校验。'
          : updated.detectedVersion
            ? `已保存。探测到版本 ${updated.detectedVersion}`
            : '已保存。',
      )
    } catch (error) {
      setSavedNote(null)
      setSaveError(String(error))
    } finally {
      setSaving(false)
    }
  }

  const saveDraft = async () => {
    if (!draft || !selectedId) return
    setSaveError(null)
    setSavedNote(null)
    // 前端镜像 Rust validate_config / validate_env：更早的可读报错，不发起 invoke
    const envError = validateEnvRows(draft.envRows)
    if (envError) {
      setSaveError(envError)
      return
    }
    if (draft.binaryKind === 'manual' && draft.manualPath.trim() === '') {
      setSaveError('手动指定的 binary 路径不能为空')
      return
    }
    await persist(selectedId, buildConfig(draft))
  }

  /** Enabled 开关：按当前草稿 + 翻转后的 enabled 即时保存。 */
  const toggleEnabled = async () => {
    if (!draft || !selectedId) return
    setSaveError(null)
    setSavedNote(null)
    await persist(selectedId, buildConfig({ ...draft, enabled: !draft.enabled }))
  }

  const pickBinary = async () => {
    const picked = await openDialog({ directory: false, multiple: false, title: '选择 harness 可执行文件' })
    if (typeof picked !== 'string') return // 用户取消
    setDraft(d => (d ? { ...d, manualPath: picked } : d))
  }

  const entry = entries?.find(e => e.id === selectedId) ?? null
  const liveCount = entry ? liveCounts[entry.id] ?? 0 : 0

  if (loadError) {
    return (
      <div className="harness-manager">
        <p className="error-banner" role="alert">Harness 列表读取失败：{loadError}</p>
      </div>
    )
  }
  if (!entries) {
    return (
      <div className="harness-manager">
        <p className="empty-hint">读取中…</p>
      </div>
    )
  }

  return (
    <div className="harness-manager">
      <div className="hm-layout">
        <div className="hm-nav">
          {entries.map(e => (
            <button
              key={e.id}
              className={`hm-nav-item${e.id === selectedId ? ' selected' : ''}`}
              onClick={() => selectHarness(e.id)}
            >
              <span className="hm-nav-label">{e.label}</span>
              <span className={`hm-state-badge ${e.state}`}>{STATE_TEXT[e.state]}</span>
            </button>
          ))}
        </div>
        {entry && draft ? (
          <div className="hm-detail">
            <div className="hm-detail-header">
              <h4>{entry.label}</h4>
              <span className={`hm-state-badge ${entry.state}`}>{STATE_TEXT[entry.state]}</span>
              {!entry.installed && <span className="hm-muted-chip">未安装</span>}
            </div>
            {/* 禁用语义呈现：仅挡新派发，存活 Agent 不受影响（U7） */}
            {!entry.enabled && liveCount > 0 && (
              <div className="info-banner" role="status">
                {liveCount} 个运行中 Agent，仅阻止新派发；终止请前往 Agents 面板
              </div>
            )}
            {saveError && <div className="error-banner" role="alert">{saveError}</div>}
            {savedNote && <p className="info-banner">{savedNote}</p>}

            <div className="hm-field">
              <span className="hm-label">启用</span>
              <div className="hm-switch-row">
                <button
                  type="button"
                  role="switch"
                  aria-checked={draft.enabled}
                  aria-label={`启用 ${entry.label}`}
                  className={`hm-switch${draft.enabled ? ' on' : ''}`}
                  disabled={saving}
                  onClick={() => void toggleEnabled()}
                >
                  <span className="hm-switch-knob" />
                </button>
                <span className="hm-hint">
                  {draft.enabled ? '已启用——可被派发' : '未启用——仅阻止新派发，不影响已运行的 Agent'}
                </span>
              </div>
            </div>

            <div className="hm-field">
              <span className="hm-label">CLI &amp; Args</span>
              <div className="chip-row">
                <button
                  className={`chip${draft.binaryKind === 'detected' ? ' active' : ''}`}
                  onClick={() => setDraft({ ...draft, binaryKind: 'detected' })}
                >
                  自动检测
                </button>
                <button
                  className={`chip${draft.binaryKind === 'manual' ? ' active' : ''}`}
                  onClick={() => setDraft({ ...draft, binaryKind: 'manual' })}
                >
                  手动指定
                </button>
              </div>
              {draft.binaryKind === 'detected' ? (
                <p className="hm-hint">
                  每次派发现场重探 PATH，不落死路径（brew 升级换位置不失效）。
                  {entry.detectedPath && (
                    <>当前：<span className="mono">{entry.detectedPath}</span></>
                  )}
                  {entry.detectedVersion && (
                    <>版本 <span className="mono">{entry.detectedVersion}</span></>
                  )}
                </p>
              ) : (
                <>
                  <div className="hm-manual-row">
                    <input
                      className="hm-input mono"
                      aria-label="手动 binary 路径"
                      placeholder="/path/to/binary"
                      value={draft.manualPath}
                      onChange={e => setDraft({ ...draft, manualPath: e.target.value })}
                    />
                    <button className="chip" onClick={() => void pickBinary()}>选择文件…</button>
                  </div>
                  {entry.state === 'notDetected' && (
                    <p className="hm-hint hm-warn">
                      未在 PATH 检测到 {entry.label}，必须手动指定可执行文件路径。
                    </p>
                  )}
                  {entry.detectedVersion && (
                    <p className="hm-hint">
                      探测：<span className="mono">{entry.detectedVersion}</span>
                    </p>
                  )}
                </>
              )}
              <textarea
                className="hm-textarea mono"
                rows={3}
                aria-label="附加参数"
                placeholder={'--model k3\n--verbose（一行一个参数）'}
                value={draft.extraArgsText}
                onChange={e => setDraft({ ...draft, extraArgsText: e.target.value })}
              />
              <span className="hm-hint">附加 CLI 参数，一行一个；保存时探测并回显版本，探测失败可保存但标「待验证」。</span>
            </div>

            <div className="hm-field">
              <span className="hm-label">Env（启动环境变量，值可为空）</span>
              {draft.envRows.map((row, i) => (
                <div className="env-row" key={i}>
                  <input
                    className="hm-input mono"
                    aria-label={`环境变量名 ${i + 1}`}
                    placeholder="KEY"
                    value={row.key}
                    onChange={e =>
                      setDraft({
                        ...draft,
                        envRows: draft.envRows.map((r, j) => (j === i ? { ...r, key: e.target.value } : r)),
                      })
                    }
                  />
                  <input
                    className="hm-input mono"
                    aria-label={`环境变量值 ${i + 1}`}
                    placeholder="值（可为空）"
                    value={row.value}
                    onChange={e =>
                      setDraft({
                        ...draft,
                        envRows: draft.envRows.map((r, j) => (j === i ? { ...r, value: e.target.value } : r)),
                      })
                    }
                  />
                  <button
                    className="chip"
                    aria-label={`删除变量 ${i + 1}`}
                    onClick={() => setDraft({ ...draft, envRows: draft.envRows.filter((_, j) => j !== i) })}
                  >
                    ✕
                  </button>
                </div>
              ))}
              <div>
                <button className="chip" onClick={() => setDraft({ ...draft, envRows: [...draft.envRows, { key: '', value: '' }] })}>
                  ＋ 添加变量
                </button>
              </div>
              <span className="hm-hint">保留键 PATH / OMT_HOME / OMT_RUNTIME_DIR 由 desktop 注入，拒绝保存。</span>
            </div>

            <div className="hm-actions">
              <button className="chip primary" disabled={saving} onClick={() => void saveDraft()}>
                {saving ? '保存中…' : '保存'}
              </button>
            </div>
          </div>
        ) : (
          <div className="hm-detail">
            <p className="empty-hint">从左侧选择一个 harness 类型</p>
          </div>
        )}
      </div>
    </div>
  )
}
