/** Matches --working-duration in thread.css. */
export const workingExitMs = 420

/** 保留最近的活动快照，直到退出动画结束。 */
export function workingTransition<T>(current: T, previous: T, visible: boolean, mounted: boolean) {
  const active = visible ? current : previous
  return { active, indicator: mounted ? active : null }
}
