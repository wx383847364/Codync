import { useRef, type PointerEvent, type ReactNode } from 'react'

const titleArea = '.sidebar-header, .top-chrome, .details-drag, .detail-drag'
const interactive = '.no-drag, button, a, input, [role="button"], [role="separator"]'

/** 聊天窗口的标题区共用一次拖动捕获，表单和工具按钮继续接收指针事件。 */
export function ChatWindowFrame({ covered, children }: { covered: boolean; children: ReactNode }) {
  const drag = useRef<{ id: number; x: number; y: number } | null>(null)
  const manual = window.codync.app.manualWindowDrag

  /** 只从窗口顶部的空白区开始拖动，菜单、弹窗及标题区按钮保留自己的交互。 */
  const start = (event: PointerEvent<HTMLDivElement>) => {
    // 系统调整位置时可能打断捕获，下一次按下必须丢弃旧起点。
    drag.current = null
    if (covered || event.button !== 0 || event.pointerType !== 'mouse' ||
      !(event.target instanceof Element) || !event.currentTarget.contains(event.target) ||
      event.target.closest(interactive) || !event.target.closest(titleArea)) return
    drag.current = { id: event.pointerId, x: event.clientX, y: event.clientY }
    event.currentTarget.setPointerCapture(event.pointerId)
  }

  /** 捕获指针后，拖出标题带也能移动窗口。 */
  const move = (event: PointerEvent<HTMLDivElement>) => {
    const active = drag.current
    if (!active || active.id !== event.pointerId) return
    if (!(event.buttons & 1)) {
      drag.current = null
      return
    }
    window.codync.app.moveChatWindow(active.x, active.y)
  }

  /** 松开、取消或失去指针捕获时都结束当前拖动。 */
  const end = () => { drag.current = null }

  return (
    <div className={`split ${covered ? 'covered' : ''} ${manual ? 'manual-window-drag' : ''}`}
      onPointerDown={manual ? start : undefined} onPointerMove={manual ? move : undefined}
      onPointerUp={manual ? end : undefined} onPointerCancel={manual ? end : undefined}
      onLostPointerCapture={manual ? end : undefined}>{children}</div>
  )
}
