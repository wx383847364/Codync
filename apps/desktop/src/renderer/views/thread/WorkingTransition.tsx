import { useLayoutEffect, useRef } from 'react'
import type { Bot, Entry } from '@shared/models'
import { usePresence } from '../../components/Overlay'
import { WorkingIndicator } from './WorkingIndicator'
import { workingExitMs, workingTransition } from './working-transition'

/** 保留上一次活动列表直至退出动画结束，群成员分别显示自己的活动。 */
export function WorkingTransition({ bots, group, entries }: { bots: Bot[]; group: boolean; entries: Entry[] }) {
  const visible = bots.length > 0
  // 初次打开会话直接显示，后续显隐和 CSS 的时长一致。
  const presence = usePresence(visible, workingExitMs, false)
  const lastActive = useRef(bots)
  const { active, indicator } = workingTransition(bots, lastActive.current, visible, presence.mounted)
  // 仅记住已渲染的活动，以便离场时保留原有标签。
  useLayoutEffect(() => { lastActive.current = active }, [active])
  return (
    <div className={`chat-working ${presence.shown ? 'shown' : ''}`} inert={!visible} aria-hidden={!visible}>
      <div className="chat-working-clip">
        <div className={group ? 'chat-working-content group-working' : 'chat-working-content'}>
          {indicator?.map((bot) => {
            // 每位成员各自展开详情，新回合自动折叠。
            return <WorkingIndicator key={`${bot.id}:${bot.startedAt ?? 0}`} bot={bot} entries={entries} group={group} />
          })}
        </div>
      </div>
    </div>
  )
}
