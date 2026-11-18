import { describe, expect, it } from 'vitest'
import { GIB, recording, recordingRoots } from '../../test/camerasFixtures'
import {
  emptyFilters, hasFilter, listSummary, matchesFilters, mergeSeen, serverQuery, sortNote, valueOptions, PAGE_SIZE,
} from './filters'
import { quotaAlerts, retentionLines, RETENTION_WORDS, rootName, toneFor, usages } from './quota'
import { ariaSort, DEFAULT_SORT, nextSort, parseSort, sortRecordings, sortValue } from './sort'
import { durationText, startedText } from './text'

describe('filters', () => {
  const early = recording(0, { name: 'alpha.flv' })
  const late = recording(5, { name: 'Bravo.flv', video_codec: 'hevc', stream: 'cam-garage', status: 'partial' })

  it('sends only the server-side filters and the page size', () => {
    const filters = { ...emptyFilters(), stream: 'cam-garage', status: 'partial', codec: 'avc', search: 'x', from: '2026-01-01' }
    expect(serverQuery(filters)).toEqual({ stream: 'cam-garage', status: 'partial', limit: PAGE_SIZE, cursor: undefined })
    expect(serverQuery(emptyFilters(), 'next')).toEqual({ stream: '', status: undefined, limit: 50, cursor: 'next' })
  })

  it('matches name search without case and trims it', () => {
    expect(matchesFilters(late, { ...emptyFilters(), search: '  bRAvo ' })).toBe(true)
    expect(matchesFilters(early, { ...emptyFilters(), search: 'bravo' })).toBe(false)
  })

  it('matches the video codec, the stream and the status exactly', () => {
    expect(matchesFilters(late, { ...emptyFilters(), codec: 'hevc' })).toBe(true)
    expect(matchesFilters(early, { ...emptyFilters(), codec: 'hevc' })).toBe(false)
    expect(matchesFilters(early, { ...emptyFilters(), stream: 'cam-garage' })).toBe(false)
    expect(matchesFilters(late, { ...emptyFilters(), status: 'partial' })).toBe(true)
  })

  it('includes both UTC days of the range', () => {
    const edge = recording(0, { started_at_unix_ms: Date.UTC(2026, 0, 3, 23, 59, 59) })
    const next = recording(0, { started_at_unix_ms: Date.UTC(2026, 0, 4, 0, 0, 0) })
    const range = { ...emptyFilters(), from: '2026-01-02', to: '2026-01-03' }
    expect(matchesFilters(edge, range)).toBe(true)
    expect(matchesFilters(next, range)).toBe(false)
    expect(matchesFilters(recording(0, { started_at_unix_ms: Date.UTC(2026, 0, 1, 23, 59, 59) }), range)).toBe(false)
  })

  it('dates a recording without a start by its modification time', () => {
    const unstarted = recording(0, { started_at_unix_ms: null, modified_at_unix_ms: Date.UTC(2026, 5, 6, 12) })
    expect(matchesFilters(unstarted, { ...emptyFilters(), from: '2026-06-06', to: '2026-06-06' })).toBe(true)
    expect(matchesFilters(unstarted, { ...emptyFilters(), to: '2026-06-05' })).toBe(false)
  })

  it('tells when any filter is set', () => {
    expect(hasFilter(emptyFilters())).toBe(false)
    expect(hasFilter({ ...emptyFilters(), search: ' ' })).toBe(false)
    expect(hasFilter({ ...emptyFilters(), codec: 'avc' })).toBe(true)
  })

  it('builds the option lists from the values seen, sorted, keeping a selected value', () => {
    const seen = mergeSeen(mergeSeen([], ['cam-b', null, 'cam-a', 'cam-b']), ['cam-c'])
    expect(seen).toEqual(['cam-a', 'cam-b', 'cam-c'])
    expect(mergeSeen(seen, ['cam-a'])).toBe(seen)
    expect(valueOptions('All streams', seen, '').map((option) => option.label)).toEqual(['All streams', 'cam-a', 'cam-b', 'cam-c'])
    expect(valueOptions('All streams', seen, 'cam-z').map((option) => option.value)).toEqual(['', 'cam-a', 'cam-b', 'cam-c', 'cam-z'])
  })

  it('says honestly how much is shown and what the sort covers', () => {
    expect(listSummary(3, 50, true)).toBe('Showing 3 of 50 loaded recordings; more are available.')
    expect(listSummary(4, 4, false)).toBe('Showing 4 of 4 recordings.')
    expect(sortNote(1)).toBe('Sorted among the 1 loaded recording.')
    expect(sortNote(50)).toBe('Sorted among the 50 loaded recordings.')
  })
})

describe('sorting', () => {
  const items = [
    recording(1, { name: 'b.flv', stream: null, duration_ms: null, size_bytes: '10', status: 'complete' }),
    recording(2, { name: 'a.flv', stream: 'cam-z', duration_ms: 5, size_bytes: '9007199254740993', status: 'recording' }),
    recording(3, { name: 'c.flv', stream: 'cam-a', duration_ms: 50, size_bytes: '900', status: 'partial' }),
  ]
  const order = (key: Parameters<typeof nextSort>[1], direction: 'ascending' | 'descending') =>
    sortRecordings(items, { key, direction }).map((item) => item.name)

  it('starts with the newest start first', () => {
    expect(DEFAULT_SORT).toEqual({ key: 'started', direction: 'descending' })
    expect(sortRecordings(items, DEFAULT_SORT).map((item) => item.name)).toEqual(['c.flv', 'a.flv', 'b.flv'])
  })

  it('sorts names, sizes beyond double precision and statuses', () => {
    expect(order('name', 'ascending')).toEqual(['a.flv', 'b.flv', 'c.flv'])
    expect(order('size', 'descending')).toEqual(['a.flv', 'c.flv', 'b.flv'])
    expect(order('status', 'ascending')).toEqual(['a.flv', 'c.flv', 'b.flv'])
  })

  it('keeps unknown streams and durations last in both directions', () => {
    expect(order('stream', 'ascending')).toEqual(['c.flv', 'a.flv', 'b.flv'])
    expect(order('stream', 'descending')).toEqual(['a.flv', 'c.flv', 'b.flv'])
    expect(order('duration', 'ascending')).toEqual(['a.flv', 'c.flv', 'b.flv'])
    expect(order('duration', 'descending')).toEqual(['c.flv', 'a.flv', 'b.flv'])
  })

  it('does not reorder its input', () => {
    const before = items.map((item) => item.name)
    sortRecordings(items, { key: 'name', direction: 'ascending' })
    expect(items.map((item) => item.name)).toEqual(before)
  })

  it('toggles the direction of the sorted column and starts others in their natural direction', () => {
    expect(nextSort(DEFAULT_SORT, 'started')).toEqual({ key: 'started', direction: 'ascending' })
    expect(nextSort(DEFAULT_SORT, 'name')).toEqual({ key: 'name', direction: 'ascending' })
    expect(nextSort(DEFAULT_SORT, 'size')).toEqual({ key: 'size', direction: 'descending' })
    expect(ariaSort(DEFAULT_SORT, 'started')).toBe('descending')
    expect(ariaSort(DEFAULT_SORT, 'name')).toBe('none')
  })

  it('round-trips the sort choice used on narrow screens', () => {
    expect(parseSort(sortValue({ key: 'size', direction: 'ascending' }))).toEqual({ key: 'size', direction: 'ascending' })
    expect(parseSort('nothing')).toBeUndefined()
  })
})

describe('quota', () => {
  const [archive, cold] = recordingRoots() as [ReturnType<typeof recordingRoots>[0], ReturnType<typeof recordingRoots>[0]]

  it('names a root by the recorders that share it', () => {
    expect(rootName(archive)).toBe('live/archive')
    expect(rootName({ ...archive, recorders: [...archive.recorders, { service: 'edge', application: 'live', recorder: 'night' }] }))
      .toBe('live/archive, live/night')
  })

  it('turns warn at 75 percent and bad at 90 percent', () => {
    expect([0, 74.9].map(toneFor)).toEqual(['ok', 'ok'])
    expect([75, 89.9].map(toneFor)).toEqual(['warn', 'warn'])
    expect([90, 100].map(toneFor)).toEqual(['bad', 'bad'])
  })

  it('measures bytes and files and says so in text', () => {
    const [bytes, files] = usages(archive)
    expect(bytes).toMatchObject({ limited: true, tone: 'ok', text: '7 GiB of 10 GiB (70%)' })
    expect(files).toMatchObject({ limited: true, tone: 'ok', text: '4 of 10 (40%)' })
    expect(usages(cold).map((item) => item.tone)).toEqual(['bad', 'bad'])
  })

  it('shows no meter but no limit when a maximum is not set', () => {
    const unlimited = usages({ ...archive, max_bytes: null, max_files: null })
    expect(unlimited.map((item) => [item.limited, item.text])).toEqual([[false, '7 GiB used, No limit'], [false, '4 used, No limit']])
  })

  it('explains retention, age and evictions', () => {
    expect(retentionLines(archive)).toEqual([
      RETENTION_WORDS.delete_oldest,
      'Finished recordings older than 7d 0h are deleted.',
      'Evicted so far: 3 files, 1.5 GiB.',
    ])
    expect(retentionLines(cold)[0]).toBe(RETENTION_WORDS.reject)
    expect(retentionLines(cold)[1]).toBe('Finished recordings are not deleted by age.')
    expect(RETENTION_WORDS.reject).toContain('quota_files or quota_bytes')
  })

  it('warns only for a reject root at 90 percent or more, and calls a full one bad', () => {
    expect(quotaAlerts([archive])).toEqual([])
    const [nearly] = quotaAlerts([{ ...cold, files: 4, max_files: 10 }])
    expect(nearly).toMatchObject({ name: 'archive/cold', tone: 'warn' })
    expect(nearly!.text).toContain('quota_bytes')
    expect(nearly!.text).toContain('Delete recordings or raise the limit in the recorder configuration.')
    const [full] = quotaAlerts([cold])
    expect(full).toMatchObject({ tone: 'bad' })
    expect(full!.text).toContain('quota_files')
    expect(quotaAlerts([{ ...archive, retention: 'delete_oldest', bytes_used: String(10 * GIB) }])).toEqual([])
  })
})

describe('text', () => {
  it('shows the start in UTC and says when it is only the modification time', () => {
    expect(startedText(recording(0))).toBe('2026-01-01 10:00:00 UTC')
    expect(startedText(recording(0, { started_at_unix_ms: null, modified_at_unix_ms: Date.UTC(2026, 0, 2) }))).toBe('2026-01-02 00:00:00 UTC (modified)')
    expect(durationText(recording(0))).toBe('30m 0s')
    expect(durationText(recording(0, { duration_ms: null }))).toBe('—')
  })
})
