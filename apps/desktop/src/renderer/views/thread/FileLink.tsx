import { useContext, useRef, useState, type ReactNode } from 'react'
import { isHTMLFileLink, type FileLinkAction, type FileLinkEditor, type FileLinkResult } from '@shared/file-links'
import { AnchoredMenu, type MenuItem } from '../../components/Overlay'
import { useTr } from '../../lib/use-language'
import { analytics } from '../../store/analytics'
import { AppContext, useStoreRef } from '../../store/context'

const editorNames: Record<FileLinkEditor, string> = { cursor: 'Open in Cursor', vscode: 'Open in Visual Studio Code' }
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
  const anchor = useRef<HTMLButtonElement>(null)
  const [menu, setMenu] = useState<{ point?: { x: number; y: number }; editors?: FileLinkEditor[] } | null>(null)

  /** 只上报固定的动作和结果。文件路径与系统错误细节留在本机。 */
  async function act(action: FileLinkAction) {
    const result: FileLinkResult = await window.codync.fileLinks.act(store.computer.id, path, action).catch(() => 'failed')
    analytics.track({ event: 'file_link_action', action, outcome: result })
    if (result in failureMessage) {
      const message = action === 'copy_contents' && result === 'unsupported'
        ? 'Copy file contents supports UTF-8 text files up to 1 MiB.'
        : failureMessage[result as keyof typeof failureMessage]
      store.setError(tr(message))
    }
  }

  /** 复制路径可离线使用，剪贴板的目标是当前设备。 */
  function copyPath() {
    window.codync.app.copy(path)
    analytics.track({ event: 'file_link_action', action: 'copy_path', outcome: 'ok' })
  }

  /** 打开方式沿用同一浮层，查找结束后若菜单已被关闭则不再弹出。 */
  async function openWith() {
    const current = menu
    const installed = await window.codync.fileLinks.editors(store.computer.id).catch(() => [])
    // 只更新发起此次查询的菜单。
    setMenu((shown) => shown && shown === current ? { ...shown, editors: installed } : shown)
  }

  /** 主菜单按电脑归属过滤动作，编辑器页保留系统默认方式。 */
  function items(): MenuItem[] {
    if (!local) return [{ title: 'Copy path', icon: 'doc.on.doc', action: copyPath }]
    if (menu?.editors) return [
      ...menu.editors.map((editor) => ({
        title: editorNames[editor], icon: 'chevron.left.forwardslash.chevron.right',
        // 仅编辑器页可将路径交给安装好的编辑器。
        action: () => void act(editor),
      })),
      { title: opensInBrowser ? 'Open in browser' : 'Open with default app', icon: 'arrow.up.right.square', action: () => void act('open') },
    ]
    return [
      { title: opensInBrowser ? 'Open in browser' : 'Open', icon: 'arrow.up.right.square', action: () => void act('open') },
      ...(window.codync.platform === 'darwin' ? [{
        title: 'Open with…', icon: 'chevron.right', keepsOpen: true,
        // 查询编辑器时保持菜单可关闭。
        action: () => void openWith(),
      }] : []),
      { title: 'Save as…', icon: 'arrow.down.to.line', divider: true, action: () => void act('save') },
      { title: 'Copy path', icon: 'doc.on.doc', action: copyPath },
      { title: 'Copy file contents', icon: 'doc.text', action: () => void act('copy_contents') },
      { title: window.codync.platform === 'darwin' ? 'Show in Finder' : 'Show in file manager',
        icon: 'folder', action: () => void act('reveal') },
    ]
  }

  return <>
    <button ref={anchor} type="button" title={path} className="md-link md-file-link"
      aria-description={opensInBrowser ? tr('Open in browser') : undefined}
      aria-haspopup={opensInBrowser ? undefined : 'menu'} aria-expanded={opensInBrowser ? undefined : menu !== null}
      onClick={() => {
        // 本机 HTML 的主操作是预览。其他文件和远程路径通过菜单选择动作。
        if (opensInBrowser) void act('open')
        else setMenu({})
      }}
      onContextMenu={(event) => {
        // 阻止消息本身的右键菜单，在指针处显示文件操作。
        event.preventDefault()
        event.stopPropagation()
        setMenu({ point: { x: event.clientX, y: event.clientY } })
      }}
    >{children}</button>
    {/* 切换编辑器页时重置键盘高亮，避免确认上一页的选择。 */}
    <AnchoredMenu key={menu?.editors ? 'editors' : 'actions'} open={menu !== null} onClose={() => { /* 关闭当前文件的操作菜单。 */ setMenu(null) }}
      point={menu?.point} anchor={anchor} items={items} />
  </>
}
