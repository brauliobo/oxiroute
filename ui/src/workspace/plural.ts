import { formatExact } from '#design/format'

export const plural = (count: number, noun: string, many = `${noun}s`): string =>
  `${formatExact(count)} ${count === 1 ? noun : many}`
