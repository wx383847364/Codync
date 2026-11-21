import { useLayoutEffect, useRef } from 'react'
import { isGroup, type Bot, type Entry } from '@shared/models'
import { IconButton } from '../../components/Controls'
import { Icon } from '../../components/Icon'
import { usePresence } from '../../components/Overlay'
import { HStack } from '../../components/Stack'
import { Text } from '../../components/Text'
import { useTr } from '../../lib/use-language'
import { useStore } from '../../store/context'
import { WorkingIndicator } from './WorkingIndicator'
import { workingExitMs, workingTransition } from './working-transition'

/** 保留上一次活动列表直至退出动画结束，群成员分别显示自己的活动。 */
export function WorkingTransition({ bots, chat, entries }: { bots: Bot[]; chat: Bot; entries: Entry[] }) {
  const store = useStore()
  const tr = useTr()
  const group = isGroup(chat)
  const controls = store.isOffline ? [] : chat.deliveryControls ?? []
  // 保留暂停和等待继续的任务行，过滤中断宽限期内尚未清掉的旧活动。
  const held = controls.filter((task) => task.status !== 'started')
  const running = bots.filter((bot) => controls.some((task) => task.status === 'started' && task.targetBotId === bot.id)
    || !held.some((task) => task.targetBotId === bot.id && (bot.startedAt ?? Infinity) <= task.startedAt))
  const visible = running.length > 0 || held.length > 0
  // 初次打开会话直接显示，后续显隐和 CSS 的时长一致。
  const presence = usePresence(visible, workingExitMs, false)
  const current = { running, held }
  const lastActive = useRef(current)
  const { active, indicator } = workingTransition(current, lastActive.current, visible, presence.mounted)
  // 仅记住已渲染的活动，以便离场时保留原有标签。
  useLayoutEffect(() => { lastActive.current = active }, [active])
  return (
    <div className={`chat-working ${presence.shown ? 'shown' : ''}`} inert={!visible} aria-hidden={!visible}>
      <div className="chat-working-clip">
        <div className={group ? 'chat-working-content group-working' : 'chat-working-content'}>
          {indicator?.running.map((bot) => {
            // 暂停按钮绑定这次执行的身份，独立于步骤的展开按钮。
            const task = controls.find((row) => row.status === 'started' && row.targetBotId === bot.id)
            return <WorkingIndicator key={`${bot.id}:${bot.startedAt ?? 0}`} bot={bot} entries={entries} group={group} action={task
              ? <IconButton title="Pause" icon="pause.fill" disabled={!!store.mismatch} onClick={() => store.setDeliveryPaused(chat.id, task.deliveryId, task.startedAt, true)} />
              : undefined} />
          })}
          {indicator?.held.map((task) => {
            // 暂停的任务用稳定身份保留，原任务摘要可以区分同一成员的多个任务。
            const name = store.bots.get(task.targetBotId)?.name ?? tr('Bot')
            const paused = task.status === 'paused'
            const status = tr(paused ? 'Paused' : 'Continuing…')
            return <div key={task.deliveryId} className="group-working-member">
              <div className="working working-with-action" role="group" aria-label={name}>
                <HStack className="working-action-row" gap={8} align="center">
                  <Icon name={paused ? 'pause.fill' : 'clock'} size={16} />
                  <Text tone="secondary" size="subheadline" truncate grow title={task.preview}>{name} · {status}{task.preview ? ` · ${task.preview}` : ''}</Text>
                  {paused ? (
                    // 继续后由 Host 更新该任务的等待或执行状态。
                    <IconButton title="Continue" icon="play.fill" disabled={!!(chat.stage?.closedAt || chat.stage?.archivedAt || store.mismatch)}
                      onClick={() => store.setDeliveryPaused(chat.id, task.deliveryId, task.startedAt, false)} />
                  ) : null}
                </HStack>
              </div>
            </div>
          })}
        </div>
      </div>
    </div>
  )
}
