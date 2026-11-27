import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { existsSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { app, ipcMain, shell } from 'electron'
import { prefs } from './host-controller'
import { identity } from './environment'
import { computerPermissionSettings, shouldSyncScreenAgent } from '../shared/computer-access'

// Codync Screen (capture + input for Remote screen): on macOS a launchd agent inside the app
// bundle (apps/screen-macos, registered through SMAppService). macOS can attribute screen
// recording to the containing app. Linux starts its helper from the host.

const serviceName = `${identity.screenLabel}.plist`
const isMac = process.platform === 'darwin'
const runCommand = promisify(execFile)

// 重新打开同一个链接可能沿用旧列表。通过 AppKit 正常退出系统设置，等旧进程退出后再定位录屏页。
const quitSystemSettings = `
ObjC.import('AppKit')
const settings = $.NSRunningApplication.runningApplicationsWithBundleIdentifier('com.apple.systempreferences').js[0]
if (settings && settings.terminate) {
  const deadline = Date.now() + 4000
  while (!settings.terminated && Date.now() < deadline) {
    $.NSRunLoop.currentRunLoop.runUntilDate($.NSDate.dateWithTimeIntervalSinceNow(0.05))
  }
}
if (settings && !settings.terminated) throw new Error('System Settings did not quit.')
`

/** 在录屏恢复后重新打开权限页，退出请求失败时仍打开该页供用户继续授权。 */
export async function reopenScreenRecordingSettings() {
  try {
    await runCommand('/usr/bin/osascript', ['-l', 'JavaScript', '-e', quitSystemSettings], { timeout: 5_000 })
  } catch (error) {
    console.warn('Could not reload macOS Screen Recording settings', error)
  }
  await shell.openExternal(computerPermissionSettings.capture)
}

/** 返回当前安装包内的助手位置，供权限引导和启动状态检查共用。 */
function screenBundlePath() {
  return join(process.resourcesPath, '..', 'Library', 'LoginItems', `${identity.screenName}.app`)
}

/** Identifies the helper binary inside this app: its path and modification date. */
function helperStamp() {
  const path = join(screenBundlePath(), 'Contents', 'MacOS', identity.screenName)
  try {
    return `${path}@${statSync(path).mtimeMs}`
  } catch {
    return `${path}@0`
  }
}

function status(): string {
  if (!isMac || !app.isPackaged) return 'not-registered'
  return app.getLoginItemSettings({ type: 'agentService', serviceName }).status ?? 'not-registered'
}

/** Registers (or removes) the agent; returns whether macOS wants the user to allow it. */
export function setScreenAgent(on: boolean): { needsApproval: boolean } {
  if (!isMac || !app.isPackaged) return { needsApproval: false }
  // Registering an enabled agent is a no-op, so a helper launchd gave up on would never start again.
  if (on && status() === 'enabled') app.setLoginItemSettings({ type: 'agentService', serviceName, openAtLogin: false })
  app.setLoginItemSettings({ type: 'agentService', serviceName, openAtLogin: on })
  prefs.set('screenHelperStamp', on ? helperStamp() : null)
  prefs.set('screenHelperResumeAfterUpdate', false)
  return { needsApproval: status() === 'requires-approval' }
}

/**
 * Keeps Codync Screen registered while the host has Remote screen on, and registers it again
 * when this app ships a different helper binary (launchd keeps running the old one).
 */
export function syncScreenAgent(enabled: boolean): { needsApproval: boolean } {
  if (!isMac || !app.isPackaged || !enabled) return { needsApproval: false }
  // A first registration belongs to explicit setup, never application startup.
  if (!shouldSyncScreenAgent(enabled, status(), prefs.get('screenHelperResumeAfterUpdate') === true)) {
    return { needsApproval: status() === 'requires-approval' }
  }
  const stale = prefs.get('screenHelperStamp') !== helperStamp()
  if (!stale && status() === 'enabled') return { needsApproval: false }
  return setScreenAgent(true)
}

/** Before the app is replaced: the old helper stops along with its job. */
export function unregisterScreenAgent() {
  if (!isMac || !app.isPackaged || status() !== 'enabled') return
  prefs.set('screenHelperResumeAfterUpdate', true)
  app.setLoginItemSettings({ type: 'agentService', serviceName, openAtLogin: false })
  prefs.set('screenHelperStamp', null)
}

/** 通过 macOS 公开命令清除当前构建变体的单项旧授权，系统仍掌握重新授权。 */
async function resetScreenPermission(permission: unknown) {
  if (!isMac || !app.isPackaged) throw new Error('Screen permission reset is available in the installed Mac app.')
  const service = permission === 'capture' ? 'ScreenCapture' : permission === 'input' ? 'Accessibility' : null
  if (!service) throw new Error('Unknown computer access permission.')
  // macOS 可能把录屏记在主程序或助手名下，辅助功能由助手持有。
  const bundles = permission === 'capture' ? [identity.appId, identity.screenLabel] : [identity.screenLabel]
  let resetAny = false
  let failure: unknown
  for (const bundle of bundles) {
    try {
      await runCommand('/usr/bin/tccutil', ['reset', service, bundle], { timeout: 10_000 })
      resetAny = true
    } catch (error) {
      // 某个目标尚未注册时继续尝试另一个，避免挡住已清理目标的后续授权。
      console.warn(`macOS ${service} reset failed for ${bundle}`, error)
      failure = error
    }
  }
  if (!resetAny) throw failure
}

export function registerScreenIPC() {
  // Renderer 只能选择权限类型，主程序限定为当前构建变体。
  ipcMain.handle('screen:resetPermission', (_e, permission) => resetScreenPermission(permission))
  ipcMain.handle('screen:state', () => ({
    available: !isMac || (app.isPackaged && existsSync(join(screenBundlePath(), 'Contents', 'MacOS', identity.screenName))),
    permissionPaths: isMac && app.isPackaged ? {
      capture: join(process.resourcesPath, '..', '..'),
      input: screenBundlePath(),
    } : undefined,
    needsApproval: status() === 'requires-approval',
    registered: status() === 'enabled',
  }))
  ipcMain.handle('screen:setAgent', (_e, on: boolean) => setScreenAgent(on))
  ipcMain.handle('screen:syncAgent', (_e, enabled: boolean) => syncScreenAgent(enabled))
}
