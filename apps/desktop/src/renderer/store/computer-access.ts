import { computerAccessStep, computerPermissionSettings, type ComputerPermission, type ScreenAgentState } from '@shared/computer-access'
import { normalizeScreen, type ScreenState } from '@shared/models'
import { Observable } from '../lib/observable'
import type { AppModel } from './app-model'

/** Read-only checks are safe on launch. Requests happen only after an explicit setup action. */
export class ComputerAccess extends Observable {
  screen: ScreenState | null = null
  agent: ScreenAgentState | null = null
  busy: ComputerPermission | 'enable' | null = null
  copiedPermission: 'capture' | 'input' | null = null
  private checkError: string | null = null
  private actionError: string | null = null
  private lastRequest: ComputerPermission | null = null
  private refreshInFlight: Promise<void> | null = null
  private app: AppModel

  constructor(app: AppModel) { super(); this.app = app }

  get step() {
    return computerAccessStep(window.codync.platform, this.app.local?.connection.kind === 'online', this.screen, this.agent)
  }

  /** 后台注册与连接都正常后，才能报告助手已就绪。 */
  get helperReady() {
    const step = this.step
    return this.screen?.enabled === true && this.screen.connected && ['capture', 'input', 'portal', 'ready'].includes(step)
  }

  get helperName() { return this.screen?.permissionApp || 'Codync Screen' }
  get computerName() { return this.app.local?.hostName || window.codync.computerName || 'Your computer' }
  get error() { return this.checkError ?? this.actionError }

  /** 同时发起的轮询共用一次读取，授权结束时可要求在旧读取完成后再查一次。 */
  async refresh(freshAfterPending = false) {
    if (this.refreshInFlight) {
      await this.refreshInFlight
      if (freshAfterPending) await this.refresh()
      return
    }
    this.refreshInFlight = this.readStatus()
    try {
      await this.refreshInFlight
    } finally {
      this.refreshInFlight = null
      this.changed()
    }
  }

  /** 读取系统和 Host 状态，并把登录项审批变化同步到托盘。 */
  private async readStatus() {
    try {
      const local = this.app.local
      this.agent = await window.codync.app.screenAgentState()
      if (this.agent.available && window.codync.platform === 'darwin') this.app.updateScreenApproval(this.agent.needsApproval)
      if (local?.client && local.connection.kind === 'online') {
        this.screen = normalizeScreen(await local.client.call<ScreenState>('screenStatus', {}))
      } else this.screen = null
      this.checkError = null
      if (this.copiedPermission && this.screen?.[this.copiedPermission]) this.copiedPermission = null
      const granted = this.lastRequest === 'portal'
        ? this.screen?.capture && this.screen.input
        : this.lastRequest && this.screen?.[this.lastRequest]
      if (granted || this.step === 'ready') this.actionError = null
    } catch (error) {
      this.screen = null
      this.checkError = message(error)
    }
  }

  async enable() {
    if (this.busy) return
    this.busy = 'enable'
    this.actionError = null
    this.changed()
    try {
      await this.app.setRemoteScreen(true)
      if (this.app.screenError) throw new Error(this.app.screenError)
      await this.refresh()
    } catch (error) { this.actionError = message(error) }
    finally { this.busy = null; this.changed() }
  }

  /** 仅恢复已启用但未就绪的 Mac 助手，不重置录屏或控制应用权限。 */
  async restartHelper() {
    const local = this.app.local
    const client = local?.client
    if (this.busy || window.codync.platform !== 'darwin' || local?.connection.kind !== 'online' || !client) return
    this.busy = 'enable'
    this.actionError = null
    this.changed()
    try {
      // 点击后复查，防止重启刚连接上的助手或重新请求已经授予的后台权限。
      const agent = await window.codync.app.screenAgentState()
      const screen = await client.call<ScreenState>('screenStatus', {})
      const step = computerAccessStep('darwin', true, screen, agent)
      if (step === 'background') {
        this.openSettings('background')
      } else if (step === 'helper') {
        const result = await this.app.registerScreenAgent(true)
        if (result.needsApproval) this.openSettings('background')
      }
      await this.refresh()
    } catch (error) { this.actionError = message(error) }
    finally { this.busy = null; this.changed() }
  }

  /** 由当前助手发起权限请求，系统决定是否显示授权窗口。 */
  async request(permission: ComputerPermission) {
    await this.requestPermission(permission)
  }

  /** 仅恢复确定缺失的 Mac 权限，避免清掉点击前后刚授予的权限。 */
  async recoverPermission(permission: 'capture' | 'input') {
    if (window.codync.platform === 'darwin') await this.requestPermission(permission, true)
  }

  /** 让授权请求和恢复流程共用忙碌状态与错误显示。 */
  private async requestPermission(permission: ComputerPermission, recover = false) {
    const client = this.app.local?.client
    if (!client || this.busy) return
    const repair = recover && permission !== 'portal' ? permission : null
    let repairChecked = false
    this.busy = permission
    this.actionError = null
    this.lastRequest = permission
    this.copiedPermission = null
    this.changed()
    try {
      // 用户点击修复后重新检查，只有当前助手明确缺少的权限才清除旧记录。
      if (repair) {
        const current = await client.call<ScreenState>('screenStatus', {})
        if (!current?.enabled || !current.connected || current.platform !== 'macos' || current[repair] !== false) {
          await this.refresh()
          return
        }
        repairChecked = true
        await window.codync.app.resetScreenPermission(repair)
      }
      const result = await client.call<ScreenState>('requestScreenPermission', { permission }, 125_000)
      await this.refresh(repair !== null)
      if (window.codync.platform === 'darwin' && permission !== 'portal'
        && result?.[permission] === false && this.screen?.[permission] !== true) {
        this.guideManualPermission(permission, repair !== null)
      }
    } catch (error) {
      this.actionError = message(error)
      // 重置或系统请求失败时仍提供手动添加，保留失败原因。
      if (repair && repairChecked) {
        await this.refresh(true)
        if (this.screen?.[repair] !== true) this.guideManualPermission(repair, true)
      }
    } finally { this.busy = null; this.changed() }
  }

  /** 手动添加时先复制当前安装路径，再打开相应的权限页。 */
  private guideManualPermission(permission: 'capture' | 'input', copyPath: boolean) {
    const path = copyPath ? this.agent?.permissionPaths?.[permission] : undefined
    if (path) {
      window.codync.app.copy(path)
      this.copiedPermission = permission
    }
    this.openSettings(permission)
  }

  openSettings(permission: keyof typeof computerPermissionSettings) {
    window.codync.app.openSettings(computerPermissionSettings[permission])
  }
}

function message(error: unknown): string {
  if (error instanceof Error) return error.message
  if (error && typeof error === 'object' && 'message' in error) return String(error.message)
  return String(error)
}
