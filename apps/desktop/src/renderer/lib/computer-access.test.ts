import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { registerHooks } from 'node:module'
import { test } from 'node:test'
import type { AppModel } from '../store/app-model'

registerHooks({ resolve(specifier, context, nextResolve) {
  if (specifier.startsWith('@shared/')) return nextResolve(new URL(`../../shared/${specifier.slice(8)}.ts`, import.meta.url).href, context)
  if (specifier.startsWith('.') && context.parentURL && !/\.[a-z]+$/.test(specifier)) {
    const url = new URL(`${specifier}.ts`, context.parentURL)
    if (existsSync(url)) return nextResolve(url.href, context)
  }
  return nextResolve(specifier, context)
} })

test('permission setup uses the local host identity even when a remote computer is selected', async () => {
  Object.assign(globalThis, { window: { codync: { computerName: 'OS computer name' } } })
  const local = { hostName: 'Local host' }
  const state = { local: local as typeof local | null, selectedStore: { hostName: 'Remote host' } }
  const { ComputerAccess } = await import('../store/computer-access.ts')
  const access = new ComputerAccess(state as unknown as AppModel)
  assert.equal(access.computerName, 'Local host')
  local.hostName = 'Renamed local host'
  assert.equal(access.computerName, 'Renamed local host')
  state.local = null
  assert.equal(access.computerName, 'OS computer name')
  window.codync.computerName = ''
  assert.equal(access.computerName, 'Your computer')
})

test('repeated checks never request permissions; explicit actions and failures stay reviewable', async () => {
  const calls: string[] = []
  let fail = false
  let capture = false
  let input = false
  let platform = 'macos'
  let resetFails = false
  let settings = ''
  Object.assign(globalThis, { window: { codync: { platform: 'darwin', app: {
    screenAgentState: async () => ({ available: true, needsApproval: false, registered: true }),
    // 模拟系统重置失败，确认界面不会继续请求已经失败的恢复。
    resetScreenPermission: async (permission: string) => {
      calls.push(`reset:${permission}`)
      if (resetFails) throw new Error('Reset denied')
    },
    openSettings: (url: string) => { settings = url },
  } } } })
  const app = { updateScreenApproval() { /* 本用例只检查录屏和控制权限。 */ }, local: { connection: { kind: 'online' }, client: { call: async (method: string) => {
    calls.push(method)
    if (method === 'requestScreenPermission' && fail) throw new Error('Permission denied')
    return { enabled: true, connected: true, platform, capture, input, permissionApp: 'CodyncDevScreen' }
  } } }, setRemoteScreen: async () => { calls.push('enable') }, screenError: null } as unknown as AppModel
  const { ComputerAccess } = await import('../store/computer-access.ts')
  const access = new ComputerAccess(app)
  await access.refresh()
  await access.refresh()
  assert.deepEqual(calls, ['screenStatus', 'screenStatus'])
  assert.equal(access.helperName, 'CodyncDevScreen')
  assert.equal(access.step, 'capture')
  access.openSettings('input')
  assert.match(settings, /Privacy_Accessibility/)
  assert.equal(access.step, 'capture')
  fail = true
  await access.request('capture')
  assert.equal(access.busy, null)
  assert.equal(access.error, 'Permission denied')
  fail = false
  // 系统未弹出授权窗口时，首次请求直接打开录屏设置。
  settings = ''
  await access.request('capture')
  assert.match(settings, /Privacy_ScreenCapture/)
  settings = ''
  capture = true
  await access.request('capture')
  assert.equal(access.error, null)
  assert.equal(access.step, 'input')
  assert.equal(settings, '')
  // 恢复操作必须先重置当前单项权限，仍缺权限时打开相应设置页。
  settings = ''
  await access.recoverPermission('input')
  assert.deepEqual(calls.slice(-3), ['reset:input', 'requestScreenPermission', 'screenStatus'])
  assert.match(settings, /Privacy_Accessibility/)
  const count = calls.length
  await access.recoverPermission('capture')
  assert.deepEqual(calls.slice(count), ['screenStatus', 'screenStatus'])
  // 录屏授权失效时只清理录屏记录，已可用的控制权限继续保留。
  capture = false
  input = true
  settings = ''
  await access.recoverPermission('capture')
  assert.deepEqual(calls.slice(-3), ['reset:capture', 'requestScreenPermission', 'screenStatus'])
  assert.match(settings, /Privacy_ScreenCapture/)
  assert.equal(access.screen?.input, true)
  input = false
  // 助手刚重连还没发送平台和状态，此时的默认 false 不能触发重置。
  const beforeReconnect = calls.length
  platform = ''
  await access.recoverPermission('input')
  assert.deepEqual(calls.slice(beforeReconnect), ['screenStatus', 'screenStatus'])
  platform = 'macos'
  resetFails = true
  await access.recoverPermission('input')
  assert.equal(calls.at(-1), 'reset:input')
  assert.equal(access.error, 'Reset denied')
  await access.enable()
  assert.ok(calls.includes('enable'))
})


// 已启用的 Mac 助手恢复只操作后台登录项，保留其它权限和 Host 设置。
test('restarts only a missing Mac helper and guides login-item approval', async () => {
  const actions: string[] = []
  let connected = true
  let registered = false
  let needsApproval = false
  let approvalOnRestart = false
  let trayNeedsApproval = false
  const screenAgentState = async () => ({ available: true, needsApproval, registered })
  Object.assign(globalThis, { window: { codync: { platform: 'darwin', app: {
    screenAgentState,
    // 模拟系统对重新注册后的批准要求。
    setScreenAgent: async (on: boolean) => {
      actions.push(`background:${on}`)
      needsApproval = approvalOnRestart
      registered = !needsApproval
      connected = false
      return { needsApproval }
    },
    openSettings: (url: string) => { actions.push(url) },
  } } } })
  const app = {
    // 同步系统设置中手动批准后的托盘状态。
    updateScreenApproval: (on: boolean) => { trayNeedsApproval = on },
    // 复用应用已有的后台服务注册入口，Host 只提供状态检查。
    registerScreenAgent: (on: boolean) => window.codync.app.setScreenAgent(on),
    local: { connection: { kind: 'online' }, client: {
    // 返回当前助手状态，并记录任何意外的 Host 修改请求。
    call: async (method: string) => {
      if (method !== 'screenStatus') actions.push(method)
      return { enabled: true, connected, platform: 'macos', capture: true, input: true }
    },
  } } } as unknown as AppModel
  const { ComputerAccess } = await import('../store/computer-access.ts')
  const access = new ComputerAccess(app)
  await access.refresh()
  assert.equal(access.step, 'helper')
  assert.equal(access.helperReady, false)
  await access.restartHelper()
  assert.deepEqual(actions, ['background:true'])
  assert.equal(access.step, 'helper')
  connected = true
  await access.restartHelper()
  assert.deepEqual(actions, ['background:true'])
  assert.equal(access.helperReady, true)

  connected = false
  approvalOnRestart = true
  await access.restartHelper()
  assert.equal(access.step, 'background')
  assert.deepEqual(actions.slice(1), ['background:true', 'x-apple.systempreferences:com.apple.LoginItems-Settings.extension'])
  await access.restartHelper()
  assert.deepEqual(actions.slice(3), ['x-apple.systempreferences:com.apple.LoginItems-Settings.extension'])
  assert.equal(trayNeedsApproval, true)
  // 用户在系统设置批准后，托盘不再显示等待批准。
  needsApproval = false
  registered = true
  connected = true
  await access.refresh()
  assert.equal(trayNeedsApproval, false)
})
