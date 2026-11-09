// A sparkline is one SVG path over the last samples of a series. Unknown samples (undefined) leave a gap instead of
// being drawn as zero; the baseline is always 0, so a flat line at the bottom means "none" and not "no scale".

export const SPARK_WIDTH = 60
export const SPARK_HEIGHT = 16
export const SPARK_SAMPLES = 60
const PAD = 1.5

export interface SparkShape {
  d: string
  /** The known samples shown, in order. */
  known: number[]
  max: number
  last: number | undefined
}

const coordinate = (value: number) => Number(value.toFixed(1))

/** The path of the last `SPARK_SAMPLES` points, or undefined when fewer than two of them are known. */
export function sparkShape(series: ReadonlyArray<number | undefined>): SparkShape | undefined {
  const values = series.slice(-SPARK_SAMPLES)
  const known = values.filter((value): value is number => value !== undefined)
  if (known.length < 2) return undefined
  const max = Math.max(...known)
  const step = (SPARK_WIDTH - 2 * PAD) / Math.max(1, SPARK_SAMPLES - 1)
  const offset = SPARK_SAMPLES - values.length
  let d = ''
  let drawing = false
  values.forEach((value, index) => {
    if (value === undefined) { drawing = false; return }
    const x = coordinate(PAD + (offset + index) * step)
    const y = coordinate(SPARK_HEIGHT - PAD - (max === 0 ? 0 : (value / max) * (SPARK_HEIGHT - 2 * PAD)))
    d += `${drawing ? 'L' : 'M'}${x} ${y}`
    drawing = true
  })
  return { d, known, max, last: values.at(-1) }
}
