import { useContext, type ReactNode } from 'react'
import { FILE_TEXT_LIMIT_MESSAGE, isHTMLFileLink, type FileLinkResult, type SharedFileAction } from '@shared/file-links'
import { useFileActionMenu } from './FileActionMenu'
import { useTr } from '../../lib/use-language'
import { analytics } from '../../store/analytics'
import { AppContext, useStoreRef } from '../../store/context'

const failureMessage = {
  missing: 'This file could not be found on this computer.',
  unsupported: 'This action is unavailable for this file.',
  failed: 'Could not complete the file action.',
}

/** 聊天文件链接提供操作菜单，本机 HTML 也可直接用浏览器打开。 */
export function FileLink({ path, children }: { path: string; children: ReactNode }) {
  const app = useContext(AppContext)
  const store = useStoreRef()
  const tr = useTr()
  const local = app?.local === store && store.isLoopback
  const opensInBrowser = local && isHTMLFileLink(path)
  const menu = useFileActionMenu({
    act: (action) => void act(action),
    editors: () => window.codync.fileLinks.editors(store.computer.id),
    opensInBrowser,
    copyOnly: !local,
  })

  /** 只上报固定的动作和结果。文件路径与系统错误细节留在本机。 */
  async function act(action: SharedFileAction) {
    if (action === 'copy_path') {
      window.codync.app.copy(path)
      analytics.track({ event: 'file_link_action', action, outcome: 'ok' })
      return
    }
    const result: FileLinkResult = await window.codync.fileLinks.act(store.computer.id, path, action).catch(() => 'failed')
    analytics.track({ event: 'file_link_action', action, outcome: result })
    if (result in failureMessage) {
      const message = action === 'copy_contents' && result === 'unsupported'
        ? FILE_TEXT_LIMIT_MESSAGE
        : failureMessage[result as keyof typeof failureMessage]
      store.setError(tr(message))
    }
  }

  return <>
    <button ref={menu.anchor} type="button" title={path} className="md-link md-file-link"
      aria-description={opensInBrowser ? tr('Open in browser') : undefined}
      aria-haspopup={opensInBrowser ? undefined : 'menu'} aria-expanded={opensInBrowser ? undefined : menu.open}
      onClick={() => opensInBrowser ? void act('open') : menu.show()}
      onContextMenu={menu.onContextMenu}
    >{children}</button>
    {menu.panel}
  </>
}
