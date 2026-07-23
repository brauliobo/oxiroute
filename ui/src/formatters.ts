const numberFormatter = new Intl.NumberFormat()

export function formatCount(value: number | string): string {
  return numberFormatter.format(typeof value === 'string' ? BigInt(value) : value)
}

export function formatBytes(value: number | string): string {
  const bytes = Number(value)
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  const amount = bytes / 1024 ** exponent
  return `${amount >= 10 || exponent === 0 ? amount.toFixed(0) : amount.toFixed(1)} ${units[exponent]}`
}
