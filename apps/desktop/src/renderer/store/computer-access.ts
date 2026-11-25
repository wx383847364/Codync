import { computerAccessStep, computerPermissionSettings, type ComputerPermission, type ScreenAgentState } from '@shared/computer-access'
import { normalizeScreen, type ScreenState } from '@shared/models'
import { Observable } from '../lib/observable'
import type { AppModel } from './app-model'

/** Read-only checks are safe on launch. Requests happen only after an explicit setup action. */
export class ComputerAccess extends Observable {
  screen: ScreenState | null = null
  agent: ScreenAgentState | null = null
  busy: ComputerPermission | 'enable' | null = null
  private checkError: string | null = null
  private actionError: string | null = null
  private lastRequest: ComputerPermission | null = null
  private refreshing = false
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

  /** 重新读取系统和 Host 状态，并同步系统设置里发生的后台批准变化。 */
  async refresh() {
    if (this.refreshing) return
    this.refreshing = true
    try {
      const local = this.app.local
      this.agent = await window.codync.app.screenAgentState()
      if (this.agent.available && window.codync.platform === 'darwin') this.app.updateScreenApproval(this.agent.needsApproval)
      if (local?.client && local.connection.kind === 'online') {
        this.screen = normalizeScreen(await local.client.call<ScreenState>('screenStatus', {}))
      } else this.screen = null
      this.checkError = null
      const granted = this.lastRequest === 'portal'
        ? this.screen?.capture && this.screen.input
        : this.lastRequest && this.screen?.[this.lastRequest]
      if (granted || this.step === 'ready') this.actionError = null
    } catch (error) {
      this.screen = null
      this.checkError = message(error)
    } finally {
      this.refreshing = false
      this.changed()
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

  /** 仅恢复确定缺失的 Mac 权限，避免清掉确认期间刚授予的权限。 */
  async recoverPermission(permission: 'capture' | 'input') {
    await this.requestPermission(permission, true)
  }

  /** 让授权请求和恢复流程共用忙碌状态与错误显示。 */
  private async requestPermission(permission: ComputerPermission, recover = false) {
    const client = this.app.local?.client
    if (!client || this.busy) return
    this.busy = permission
    this.actionError = null
    this.lastRequest = permission
    this.changed()
    try {
      // 用户确认后先清除失效记录，再由当前助手向 macOS 请求授权。
      if (recover && permission !== 'portal') {
        const current = await client.call<ScreenState>('screenStatus', {})
        if (!current?.enabled || !current.connected || current.platform !== 'macos' || current[permission] !== false) {
          await this.refresh()
          return
        }
        await window.codync.app.resetScreenPermission(permission)
      }
      const result = await client.call<ScreenState>('requestScreenPermission', { permission }, 125_000)
      await this.refresh()
      // 系统可能直接拒绝弹窗，仍未授权时打开对应页面供手动添加。
      if (window.codync.platform === 'darwin' && permission !== 'portal'
        && result?.[permission] === false && this.screen?.[permission] !== true) this.openSettings(permission)
    } catch (error) {
      this.actionError = message(error)
    } finally { this.busy = null; this.changed() }
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
