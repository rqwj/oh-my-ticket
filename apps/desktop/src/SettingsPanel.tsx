/**
 * U10 settings panel: daemon status projection + home selector/declare
 * flow. Admin-only actions render ONLY when the credential's operations
 * include the admin family (AE12: no grant → no render).
 */
import { useEffect, useState } from 'react'
import { ask, open as openDialog } from '@tauri-apps/plugin-dialog'
import {
  appVersion,
  checkoutValid,
  daemonStatus,
  harnessDetect,
  installDshPlugin,
  omtCall,
  type DaemonStatus,
} from './bridge'
import type { HomeInfo } from './types'
import type { KnownHome } from './store'
import { NOT_A_WORKSPACE_HINT, resolveHomeFromPickedDir } from './workspacePath'
import { workspaceRootOf } from './homePath'
import { HarnessSelect, type HarnessConfig } from './HarnessSelect'

interface Props {
  homes: HomeInfo[]
  knownHomes: KnownHome[]
  homeDir?: string
  harnessByHome: Record<string, HarnessConfig>
  onSaveHarness: (home: HomeInfo, config: HarnessConfig | null) => Promise<void>
  activeHome: HomeInfo | null
  onSelectHome: (home: HomeInfo) => void
  onDeclare: (path: string) => Promise<string | null>
}

export function SettingsPanel({ homes, knownHomes, homeDir, harnessByHome, onSaveHarness, activeHome, onSelectHome, onDeclare }: Props) {
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [declareMessage, setDeclareMessage] = useState<string | null>(null)
  const [isAdmin, setIsAdmin] = useState(false)
  const [picking, setPicking] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [installMessage, setInstallMessage] = useState<string | null>(null)

  useEffect(() => {
    void daemonStatus().then(setStatus).catch(() => {})
    // Capability probe: an admin-family call that fails FORBIDDEN tells us
    // the credential lacks the grant — admin actions stay hidden (AE12).
    omtCall('home/reindex', { homeId: activeHome?.homeId ?? '', dryRun: true })
      .then(() => setIsAdmin(true))
      .catch(error => setIsAdmin(!String(error).includes('FORBIDDEN')))
  }, [activeHome?.homeId])

  const declare = async (homePath: string) => {
    const error = await onDeclare(homePath)
    if (error !== null && (error.includes('不存在') || error.includes('unreadable') || error.includes('not a directory') || error.includes('INVALID_INPUT'))) {
      setDeclareMessage(NOT_A_WORKSPACE_HINT)
      return
    }
    setDeclareMessage(error ?? `已添加 ${homePath}`)
  }

  const pickWorkspace = async () => {
    setPicking(true)
    try {
      const picked = await openDialog({ directory: true, multiple: false, title: '选择要添加的 workspace 目录' })
      if (typeof picked !== 'string') return // 用户取消
      await declare(resolveHomeFromPickedDir(picked))
    } finally {
      setPicking(false)
    }
  }

  /**
   * 「安装 DSH 插件」流程：先探测全局 dsh；未装则弹窗问是否走 dsh 开发
   * 环境（源码 checkout）——拒绝即收场，接受则选目录并校验。安装包版本
   * 与桌面 app 版本 lockstep（统一版本号），重启 dsh web 后生效。
   */
  const installPlugin = async () => {
    setInstalling(true)
    setInstallMessage(null)
    try {
      const version = await appVersion()
      const detected = await harnessDetect('dsh')
      let mode: 'global' | 'dev' = 'global'
      let checkoutDir: string | undefined
      if (!detected.installed) {
        const useDev = await ask('未检测到全局安装的 dsh。是否使用 dsh 开发环境（源码 checkout）安装插件？', {
          title: '未检测到 dsh',
          kind: 'info',
          okLabel: '是',
          cancelLabel: '否',
        })
        if (!useDev) return // 用户选择「否」→ 关闭弹窗收场
        const picked = await openDialog({ directory: true, multiple: false, title: '选择 dsh 开发环境目录（deepseek-harness checkout）' })
        if (typeof picked !== 'string') return // 用户取消选目录
        const valid = await checkoutValid(picked)
        if (!valid.valid) {
          setInstallMessage('所选目录不是 dsh 开发环境（缺 pnpm-workspace.yaml / apps / packages）')
          return
        }
        mode = 'dev'
        checkoutDir = picked
      }
      setInstallMessage(`安装中… dsh-oh-my-ticket@${version}（${mode === 'global' ? '全局 dsh' : '开发模式'}）`)
      const result = await installDshPlugin({ mode, checkoutDir, packageName: 'dsh-oh-my-ticket', version })
      setInstallMessage(
        result.ok
          ? `已安装 dsh-oh-my-ticket@${version} 到 dsh「web」profile（${mode === 'global' ? '全局 dsh' : '开发模式'}）。重启 dsh web 后生效。`
          : `安装失败：${result.output || '命令非零退出'}`,
      )
    } catch (error) {
      setInstallMessage(`安装失败：${String(error)}`)
    } finally {
      setInstalling(false)
    }
  }

  return (
    <div className="settings-panel">
      <section>
        <h3>Daemon</h3>
        {status ? (
          <dl className="status-grid">
            <dt>状态</dt><dd>{status.running ? '运行中' : '未运行'}</dd>
            <dt>pid</dt><dd>{status.pid ?? '—'}</dd>
            <dt>代际</dt><dd>{status.generation ?? '—'}</dd>
            <dt>endpoint</dt><dd className="mono">{status.endpoint ?? '—'}</dd>
          </dl>
        ) : (
          <p className="empty-hint">读取中…</p>
        )}
      </section>
      <section>
        <div className="section-header">
          <h3>DSH 集成</h3>
          <button className="chip active" disabled={installing} onClick={() => void installPlugin()}>
            {installing ? '安装中…' : '安装 DSH 插件…'}
          </button>
        </div>
        <p className="empty-hint">
          把 oh-my-ticket 插件装进本机 dsh（web profile）。版本与本桌面 app 保持一致；
          未装全局 dsh 时可改用 dsh 源码开发环境安装。
        </p>
        {installMessage && <p className="info-banner">{installMessage}</p>}
      </section>
      <section>
        <div className="section-header">
          <h3>Workspaces</h3>
          <button className="chip active" disabled={picking} onClick={() => void pickWorkspace()}>
            {picking ? '选择中…' : '＋ 添加 workspace…'}
          </button>
        </div>
        <div className="home-list">
          {homes.map(home => (
            <button
              key={home.homeId}
              className={`home-row${activeHome?.homeId === home.homeId ? ' selected' : ''}`}
              onClick={() => onSelectHome(home)}
            >
              <span className="home-row-main">
                <span className="chip">{home.kind ?? 'workspace'}</span>
                <span className="mono home-path">{workspaceRootOf(home.path, homeDir, home.name ?? home.homeId)}</span>
                {activeHome?.homeId === home.homeId && <span className="row-check">✓</span>}
              </span>
              <span className="home-row-sub">
                <HarnessSelect home={home} config={harnessByHome[home.homeId]} onSave={onSaveHarness} />
              </span>
            </button>
          ))}
        </div>
        {knownHomes.filter(k => !k.open).length > 0 && (
          <div className="known-section">
            <h4>已知未开（点击即添加）</h4>
            {knownHomes.filter(k => !k.open).map(known => (
              <button
                key={known.path}
                className="home-row"
                disabled={known.missing}
                title={known.missing ? '目录已不存在（被移动/删除）' : known.path}
                onClick={() => void onDeclare(known.path).then(err => setDeclareMessage(err ?? `已收录 ${known.name}`))}
              >
                <span className="chip">{known.kind}</span>
                <span className="mono">{workspaceRootOf(known.path, homeDir, known.name)}</span>
                {known.missing && <span className="chip">已缺失</span>}
              </button>
            ))}
          </div>
        )}
        {declareMessage && <p className="info-banner">{declareMessage}</p>}
      </section>
      {isAdmin && (
        <section>
          <h3>Admin</h3>
          <p className="empty-hint">管理操作区（凭据已授权 admin 族）</p>
        </section>
      )}
    </div>
  )
}
