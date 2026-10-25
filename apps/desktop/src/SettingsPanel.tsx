/**
 * U1 全局设置面板（R8）：Daemon、DSH 集成、Harnesses（U7）、派发策略（A8）。
 * 原设置页里的 Workspaces 列表已移到左侧工作区图标栏（R1/R3）；全局设置作
 * 为内容区整页由图标栏底部的 ⚙ 打开。Admin-only actions render ONLY when
 * the credential's operations include the admin family (AE12: no grant →
 * no render).
 *
 * U1b：旧 dsh harness 条目的 per-home 提示（迁移不静默丢弃）保留。§64
 * 遗留：按 home 逐条提示（不只看当前 home）——只要该 home 还开着就可见。
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
import { workspaceNameOf } from './workspaceList'
import { HarnessManagerTab } from './HarnessManagerTab'
import { ExecPolicySection } from './ExecPolicySection'

/** 设置页内层分段（U7 + A8）：通用 / Harnesses（R-A 一等区）/ 派发策略。 */
type SettingsSection = 'general' | 'harnesses' | 'policy'

interface Props {
  /** HarnessManagerTab 跨 home 汇总 agent 数据用 */
  homes: HomeInfo[]
  /** U1b：旧 dsh harness 条目的 home 集合（迁移不静默丢弃，提示条） */
  legacyDshByHome: Record<string, boolean>
  activeHome: HomeInfo | null
}

export function SettingsPanel({ homes, legacyDshByHome, activeHome }: Props) {
  const [status, setStatus] = useState<DaemonStatus | null>(null)
  const [isAdmin, setIsAdmin] = useState(false)
  const [installing, setInstalling] = useState(false)
  const [installMessage, setInstallMessage] = useState<string | null>(null)
  const [section, setSection] = useState<SettingsSection>('general')

  useEffect(() => {
    void daemonStatus().then(setStatus).catch(() => {})
    // Capability probe: an admin-family call that fails FORBIDDEN tells us
    // the credential lacks the grant — admin actions stay hidden (AE12).
    omtCall('home/reindex', { homeId: activeHome?.homeId ?? '', dryRun: true })
      .then(() => setIsAdmin(true))
      .catch(error => setIsAdmin(!String(error).includes('FORBIDDEN')))
  }, [activeHome?.homeId])

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
      {/* U7：内层分段——R-A Harnesses 是设置页一等区（非弹窗）；A8 加派发策略 */}
      <div className="inner-tabs">
        <button className={`inner-tab${section === 'general' ? ' active' : ''}`} onClick={() => setSection('general')}>
          通用
        </button>
        <button className={`inner-tab${section === 'harnesses' ? ' active' : ''}`} onClick={() => setSection('harnesses')}>
          Harnesses
        </button>
        <button className={`inner-tab${section === 'policy' ? ' active' : ''}`} onClick={() => setSection('policy')}>
          派发策略
        </button>
      </div>
      {section === 'harnesses' ? (
        <HarnessManagerTab homes={homes} />
      ) : section === 'policy' ? (
        <ExecPolicySection />
      ) : (
        <>
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
          {/* U1b / §64 遗留：旧 dsh harness 条目保留在 bag，不静默丢弃
              （dsh 接入属后续版本）——按 home 逐条提示，不只看当前 home */}
          {homes
            .filter(h => legacyDshByHome[h.homeId])
            .map(h => (
              <p key={h.homeId} className="info-banner">
                「{workspaceNameOf(h.path ?? '')}」：dsh harness 后续版本支持，当前配置已保留
              </p>
            ))}
          {isAdmin && (
            <section>
              <h3>Admin</h3>
              <p className="empty-hint">管理操作区（凭据已授权 admin 族）</p>
            </section>
          )}
        </>
      )}
    </div>
  )
}
