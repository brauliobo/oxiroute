import { snapshotUrl } from './poll'

const below = (pathname: string, name: string) => `${pathname.replace(/\/+$/, '')}/${name}`

/** The HAProxy-style CSV export of the page at `pathname`. */
export const csvUrl = (pathname: string) => below(pathname, 'stats.csv')

export const jsonUrl = snapshotUrl
