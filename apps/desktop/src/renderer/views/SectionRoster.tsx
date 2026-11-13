import { useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { moveInOrder } from '../lib/computer-roster'
import { pointerDrag, type Point } from '../lib/pointer-drag'
import { rosterGroups, stageRoster } from '../lib/roster-sections'
import { useSlide } from '../lib/slide'
import type { RosterItem } from '../store/app-model'
import { RosterLabel, RosterSection } from './RosterSection'

import { useTr } from '../lib/use-language'

interface Drag { name: string; order: string[]; at: Point; grab: Point; width: number }

/** Sections move as blocks within one computer, including the unsectioned Bots group. */
export function SectionRoster({ computerId, items, compact, folded, order, save, toggle, onContext, row }: {
  computerId: string
  items: RosterItem[]
  compact: boolean
  folded: string[]
  order: string[]
  save: (names: string[]) => void
  toggle: (name: string) => void
  onContext: (name: string, x: number, y: number) => void
  row: (item: RosterItem, members?: RosterItem[]) => ReactNode
}) {
  const [drag, setDrag] = useState<Drag | null>(null)
  const current = useRef<Drag | null>(null)
  const container = useRef<HTMLDivElement>(null)
  const ghost = useRef<HTMLDivElement>(null)
  const tr = useTr()
  const [archives, setArchives] = useState<string[]>([])
  const { roots, children } = stageRoster(items)
  const groupedItems = roots.map((item) => item.bot.stage?.archivedAt && item.bot.pinned
    ? { ...item, bot: { ...item.bot, pinned: false } } : item)
  const groups = rosterGroups(groupedItems, folded, order)
  /** 在项目底部提供独立的归档入口。 */
  const projectRows = (name: string, rows: RosterItem[]) => {
    const active = rows.filter(({ bot }) => !bot.stage?.archivedAt)
    const archived = rows.filter(({ bot }) => bot.stage?.archivedAt)
    const originals = rows.filter(({ bot }) => bot.kind !== 'group')
    const groups = active.filter(({ bot }) => bot.kind === 'group')
    const open = archives.includes(name)
    return <>
      {groups.map((item) => row(item, children(item)))}
      {!!originals.length && !!(groups.length || archived.length) && !compact && <div className="stage-project-label">{tr('Project bots')}</div>}
      {originals.map((item) => row(item))}
      {!!archived.length && <RosterSection name={tr('Archived stages')} count={archived.length} unread={0} compact={compact}
        folded={!open} toggle={() => setArchives((list) => open ? list.filter((key) => key !== name) : [...list, name])}>
        {open ? archived.map((item) => row(item, children(item))) : null}
      </RosterSection>}
    </>
  }
  const byName = new Map(groups.map((group) => [group.name, group]))
  const custom = groups.some((group) => group.name !== '')
  const names = groups.map((group) => group.name)
  const shown = drag?.order ?? names
  const update = (next: Drag | null) => { current.current = next; setDrag(next) }
  const heading = (name: string) => [...container.current?.querySelectorAll<HTMLElement>('[data-roster-group]') ?? []]
    .find((element) => element.dataset.rosterGroup === name)?.querySelector<HTMLElement>('[data-section-heading]')
  const move = (name: string, offset: number) => {
    const target = names[names.indexOf(name) + offset]
    if (target !== undefined) save(moveInOrder(names, name, target))
  }
  const handlers = (name: string) => pointerDrag({
    start(at) {
      const box = heading(name)?.getBoundingClientRect()
      if (box) update({ name, order: names, at, grab: { x: at.x - box.left, y: at.y - box.top }, width: box.width })
    },
    over(at, under) {
      const held = current.current
      if (!held) return
      if (ghost.current) ghost.current.style.translate = `${at.x - held.grab.x}px ${at.y - held.grab.y}px`
      const target = under?.closest<HTMLElement>('[data-roster-group]')
      if (!target || !container.current?.contains(target) || target.getAnimations().length) return
      const name = target.dataset.rosterGroup!
      if (name !== held.name) update({ ...held, at, order: moveInOrder(held.order, held.name, name) })
    },
    drop() { if (current.current) save(current.current.order); update(null) },
    cancel() { update(null) },
  })
  const label = (name: string, preview = false) => {
    const group = byName.get(name)
    if (!group) return null
    const props = preview ? {} : { drag: handlers(name), move: (offset: number) => move(name, offset) }
    return name === '' ? <RosterLabel compact={compact} {...props} /> : <RosterSection name={name} count={group.count}
      unread={group.unread} compact={compact}
      folded={drag !== null || folded.includes(name)} toggle={() => toggle(name)}
      onContext={(x, y) => onContext(name, x, y)} {...props}>
      {preview ? null : projectRows(name, group.items)}
    </RosterSection>
  }
  return <div ref={container} data-section-computer={computerId}>
    {groupedItems.filter((item) => item.bot.pinned).map((item) => row(item, item.bot.kind === 'group' ? children(item) : undefined))}
    {shown.map((name) => <SectionBlock key={name} name={name} lifted={drag?.name === name}>
      {name === '' && !custom ? null : label(name)}
      {name === '' ? <div className={`computer-roster-content ${drag ? 'collapsed' : ''}`} inert={drag !== null} aria-hidden={drag !== null}>
        <div>{projectRows('', byName.get('')?.items ?? [])}</div>
      </div> : null}
    </SectionBlock>)}
    {drag ? createPortal(<div ref={ghost} className="roster-ghost" style={{ width: drag.width,
      translate: `${drag.at.x - drag.grab.x}px ${drag.at.y - drag.grab.y}px` }}>
      <div className="roster-ghost-card">{label(drag.name, true)}</div>
    </div>, document.body) : null}
  </div>
}

function SectionBlock({ name, lifted, children }: { name: string; lifted: boolean; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null)
  useSlide(ref)
  return <div ref={ref} data-roster-group={name} className={`computer-roster-section ${lifted ? 'lifted' : ''}`}>{children}</div>
}
