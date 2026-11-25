import type { ReactNode } from 'react'
import type { ComputerAccess } from '../../store/computer-access'
import type { AccessPage } from '../../lib/access-onboarding'
import { Button, Spinner } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { useTr } from '../../lib/use-language'

export function AccessSteps({ access, selection }: { access: ComputerAccess; selection?: AccessPage }) {
  const tr = useTr()
  const { screen, step, busy, helperReady } = access
  const mac = window.codync.platform === 'darwin'
  const windows = window.codync.platform === 'win32'
  const ready = step === 'ready'
  const canRequest = ['capture', 'input', 'portal', 'ready'].includes(step)
  const request = (permission: 'capture' | 'input' | 'portal', title: string) => (
    <Button kind={step === permission ? 'primary' : 'secondary'} disabled={!canRequest || busy !== null} onClick={() => void access.request(permission)}>
      {busy === permission ? <><Spinner /> {tr('Waiting for permission…')}</> : title}
    </Button>
  )
  /** 仅在助手运行但实际未获授权时提供恢复入口。 */
  const recover = (permission: 'capture' | 'input') => mac && access.agent?.available && helperReady && canRequest && screen?.[permission] === false
    ? <Button kind="secondary" disabled={busy !== null} onClick={() => { /* 用户明确点击修复，只重新请求此项权限。 */ void access.recoverPermission(permission) }}>
        {tr('Repair and request access again')}
      </Button> : null
  return (
    <>
      {windows ? <AccessRow large={selection !== undefined} title="Computer control" icon="cursorarrow.motionlines" detail="Bots can use apps on this computer. Remote screen viewing from your phone isn’t available on Windows yet." done={ready} /> : <>
        {(!selection || selection === 'helper') ? <AccessRow large={selection !== undefined} title="Codync Screen" icon="desktopcomputer" detail="Runs in the background to share your screen and connect your controls." done={helperReady} active={['enable', 'helper', 'background'].includes(step)} status={step === 'background' ? 'Needs approval' : undefined}>
          {['enable', 'helper'].includes(step) ? <Button disabled={busy !== null} onClick={() => {
            // Mac 助手恢复只重新注册后台服务，首次开启和 Linux 继续走 Host 设置。
            void (mac && step === 'helper' ? access.restartHelper() : access.enable())
          }}>{busy === 'enable' ? <><Spinner /> {tr('Starting…')}</> : tr(step === 'helper' ? 'Start again' : 'Enable Codync Screen')}</Button> : null}
          {step === 'background' ? <><p className="computer-access-inline-note">{tr('Allow Codync to run in the background in Login Items.')}</p><Button onClick={() => access.openSettings('background')}>{tr('Open Login Items')} <Icon name="arrow.up.right.square" size={10} /></Button></> : null}
        </AccessRow> : null}
        {mac ? <>
          {(!selection || selection === 'capture') ? <AccessRow large={selection !== undefined} title="Screen recording" icon="record.circle" detail="Lets Codync see your screen, including anything visible in your apps." done={helperReady && screen?.capture === true} active={step === 'capture'}>
            {canRequest ? <SettingsHint path={access.agent?.permissionPaths?.capture} alternative={access.agent?.permissionPaths?.input} app="Codync" copied={access.copiedPermission === 'capture'} /> : null}
            {canRequest ? request('capture', 'Allow screen recording') : null}
            {recover('capture')}
            {canRequest ? <SettingsLink action={() => access.openSettings('capture')} /> : null}
          </AccessRow> : null}
          {(!selection || selection === 'input') ? <AccessRow large={selection !== undefined} title="Control apps" icon="cursorarrow.motionlines" detail="Lets Codync click, type and interact with apps on your behalf." done={helperReady && screen?.input === true} active={step === 'input'}>
            {canRequest ? <SettingsHint path={access.agent?.permissionPaths?.input} app={access.helperName} copied={access.copiedPermission === 'input'} /> : null}
            {canRequest ? request('input', 'Allow computer control') : null}
            {recover('input')}
            {canRequest ? <SettingsLink action={() => access.openSettings('input')} /> : null}
          </AccessRow> : null}
        </> : (!selection || selection === 'portal') ? <AccessRow large={selection !== undefined} title="Screen sharing & control" icon="cursorarrow.motionlines" detail="Choose a screen and allow mouse and keyboard access. You can stop sharing at any time." done={helperReady && screen?.capture === true && screen.input} active={step === 'portal'}>
          {canRequest ? request('portal', 'Choose screen and allow control') : null}
        </AccessRow> : null}
      </>}
    </>
  )
}

function AccessRow({ title, icon, detail, done, active = false, status, children, large = false }: {
  title: string; icon: string; detail: string; done: boolean; active?: boolean; status?: string; children?: ReactNode; large?: boolean
}) {
  const tr = useTr()
  const label = tr(done ? 'Ready' : status ?? (active ? 'Needs access' : 'Waiting'))
  return <div className={`computer-access-row ${done ? 'granted' : active ? 'current' : 'waiting'}`}>
    <span className="computer-access-badge" role="img" aria-label={label} title={label}>
      {done ? <Icon name="checkmark" size={large ? 26 : 12} weight="bold" /> : <Icon name={icon} size={large ? 26 : 15} />}
    </span>
    <div className="computer-access-row-body">
      <h2>{tr(title)}</h2>
      <p>{tr(detail)}</p>
      {!done ? <div className="computer-access-actions">{children}</div> : null}
    </div>
  </div>
}

/** 显示当前安装的授权目标，方便从系统设置手动添加。 */
function SettingsHint({ app, path, alternative, copied }: { app: string; path?: string; alternative?: string; copied: boolean }) {
  const tr = useTr()
  const name = path?.split('/').at(-1) || app
  return <div className="computer-access-inline-note">
    <p>{tr('In System Settings, look for')} <strong>{name}</strong>. {tr('If it is missing, click + and select this app.')} {tr('Return here after granting access; if asked, quit and reopen Codync.')}</p>
    {copied ? <p role="status">{tr('Path copied for {app}. Click +, then press ⌘⇧G and paste the path.').replace('{app}', name)}</p> : null}
    {path ? <PermissionPath path={path} /> : null}
    {alternative ? <>
      <p>{tr('If macOS names the screen helper, select it instead.')}</p>
      <PermissionPath path={alternative} />
    </> : null}
  </div>
}

/** 显示可选中文件路径，并复用桌面的原生剪贴板。 */
function PermissionPath({ path }: { path: string }) {
  const tr = useTr()
  return <div className="computer-access-path">
    <code>{path}</code>
    <Button kind="secondary" onClick={() => { /* 复制路径供系统文件选择器跳转。 */ window.codync.app.copy(path) }}>{tr('Copy path')}</Button>
  </div>
}

function SettingsLink({ action }: { action: () => void }) {
  const tr = useTr()
  return <Button kind="secondary" onClick={action}>{tr('Open System Settings')} <Icon name="arrow.up.right.square" size={10} /></Button>
}
