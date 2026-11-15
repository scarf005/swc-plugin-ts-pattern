export const getWheelStep = (event: { deltaX: number; deltaY: number }): number => {
  const delta = Math.abs(event.deltaY) >= Math.abs(event.deltaX) ? event.deltaY : event.deltaX
  return delta === 0 ? 0 : delta > 0 ? 1 : -1
}

export const cycleValue = <T>(options: readonly T[], current: T, step: number): T | undefined => {
  const index = options.indexOf(current)
  if (index < 0 || step === 0 || options.length === 0) return undefined
  return options[(index + step + options.length) % options.length]
}
