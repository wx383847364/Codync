import { useEffect, useRef, useState, type MouseEvent } from 'react'
import type { FileLinkEditor, SharedFileAction } from '@shared/file-links'
import { AnchoredMenu, type MenuItem, type ReactionPick } from '../../components/Overlay'

const editorNames: Record<FileLinkEditor, string> = { cursor: 'Open in Cursor', vscode: 'Open in Visual Studio Code' }
// File cards can offer a direct editor shortcut. Take a snapshot at menu open so rows
// never move under a pointer or an active keyboard selection.
let knownEditors: FileLinkEditor[] | undefined
let editorLookup: Promise<void> | undefined

type Point = { x: number; y: number }

interface FileMenuOptions {
  act(action: SharedFileAction): void
  editors(): Promise<FileLinkEditor[]>
  copyOnly?: boolean
  opensInBrowser: boolean
  quickEditor?: boolean
  messageItems?: () => MenuItem[]
  reactions?: ReactionPick
}

/** File cards and path links use the same menu, with actions provided by their respective file sources. */
export function useFileActionMenu({ act, editors, copyOnly, opensInBrowser, quickEditor, messageItems, reactions }: FileMenuOptions) {
  const anchor = useRef<HTMLButtonElement>(null)
  const [menu, setMenu] = useState<{ id: symbol; point?: Point; installed?: FileLinkEditor[]; choosingEditor?: boolean } | null>(null)
  const canChooseEditor = !copyOnly && window.codync.platform === 'darwin'

  useEffect(() => {
    if (canChooseEditor && quickEditor) {
      editorLookup ??= window.codync.files.editors().then((installed) => { knownEditors = installed }, () => { editorLookup = undefined })
    }
  }, [canChooseEditor, quickEditor])

  function open(point?: Point) {
    setMenu({ id: Symbol(), point, installed: quickEditor ? knownEditors : undefined })
  }

  async function openWith() {
    const current = menu
    const installed = await editors().catch(() => [])
    if (quickEditor) knownEditors = installed
    setMenu((shown) => shown && shown.id === current?.id ? { ...shown, installed, choosingEditor: true } : shown)
  }

  function editorItem(editor: FileLinkEditor): MenuItem {
    return { title: editorNames[editor], icon: 'square.and.pencil', action: () => act(editor) }
  }

  function items(): MenuItem[] {
    const copyPath: MenuItem = { title: 'Copy path', icon: 'doc.on.doc', action: () => act('copy_path') }
    if (copyOnly) return [copyPath]
    if (menu?.choosingEditor) return [
      ...(menu.installed ?? []).map(editorItem),
      { title: opensInBrowser ? 'Open in browser' : 'Open with default app', icon: 'arrow.up.right.square', action: () => act('open') },
    ]
    const quick = quickEditor ? menu?.installed?.[0] : undefined
    return [
      { title: opensInBrowser ? 'Open in browser' : 'Open', icon: 'arrow.up.right.square', action: () => act('open') },
      ...(quick ? [editorItem(quick)] : []),
      ...(canChooseEditor ? [{ title: 'Open with…', icon: 'chevron.right', keepsOpen: true, action: () => void openWith() }] : []),
      { title: 'Save as…', icon: 'arrow.down.to.line', divider: true, action: () => act('save') },
      copyPath,
      { title: 'Copy file contents', icon: 'doc.text', action: () => act('copy_contents') },
      { title: window.codync.platform === 'darwin' ? 'Show in Finder' : 'Show in file manager', icon: 'folder', action: () => act('reveal') },
      ...(messageItems?.() ?? []).map((item, index) => index === 0 ? { ...item, divider: true } : item),
    ]
  }

  return {
    anchor,
    open: menu !== null,
    show: () => open(),
    onContextMenu: (event: MouseEvent) => {
      event.preventDefault()
      event.stopPropagation()
      open(event.clientX || event.clientY ? { x: event.clientX, y: event.clientY } : undefined)
    },
    panel: <AnchoredMenu key={menu?.choosingEditor ? 'editors' : 'actions'} open={menu !== null} onClose={() => setMenu(null)}
      point={menu?.point} anchor={anchor} items={items} reactions={menu?.choosingEditor ? undefined : reactions} />,
  }
}
