import {
  createSignal,
  createMemo,
  createResource,
  createEffect,
  Show,
  For,
} from 'solid-js'
import { useParams, useSearchParams, A } from '@solidjs/router'
import { createVirtualizer } from '@tanstack/solid-virtual'
import { query } from '../lib/graphql'
import { JsonTree } from '../lib/json-tree'
import type { Session } from '../lib/types'
import styles from './RawLogView.module.css'

const PAGE_SIZE = 200

const LOG_LINES_QUERY = `query ($id: String!, $offset: Int!, $limit: Int!) {
  sessionLogLines(id: $id, offset: $offset, limit: $limit) {
    lines { lineNumber content }
    totalLines
  }
}`

const SESSION_INFO_QUERY = `query ($id: String!) {
  sessionInfo(id: $id) { id filePath }
}`

const RAW_LOG_QUERY = `query ($id: String!) {
  sessionRawLog(id: $id)
}`

interface LogLine {
  lineNumber: number
  content: string
}

/** Minimal parse to extract type, uuid, timestamp for the summary row */
function parseSummary(raw: string): {
  type: string
  uuid: string
  timestamp: string
} {
  let type = ''
  let uuid = ''
  let timestamp = ''
  // Quick regex extractions — cheaper than JSON.parse for summary
  const tm = raw.match(/"type"\s*:\s*"([^"]*)"/)
  if (tm) type = tm[1]
  const um = raw.match(/"uuid"\s*:\s*"([^"]*)"/)
  if (um) uuid = um[1]
  const tsm = raw.match(/"timestamp"\s*:\s*"([^"]*)"/)
  if (tsm) timestamp = tsm[1]
  return { type, uuid, timestamp }
}

function badgeClass(type: string): string {
  switch (type) {
    case 'user':
      return styles['type-user']
    case 'assistant':
      return styles['type-assistant']
    case 'system':
      return styles['type-system']
    case 'progress':
      return styles['type-progress']
    default:
      return styles['type-other']
  }
}

function formatTimestamp(ts: string): string {
  if (!ts) return ''
  try {
    const d = new Date(ts)
    return d.toLocaleTimeString()
  } catch {
    return ts
  }
}

export default function RawLogView() {
  const params = useParams<{ id: string }>()
  const [searchParams] = useSearchParams<{ uuid?: string }>()

  let scrollRef!: HTMLDivElement

  // Line cache: lineNumber -> raw content
  const [lineCache, setLineCache] = createSignal<Map<number, string>>(new Map())
  const [totalLines, setTotalLines] = createSignal(0)
  const [expandedLines, setExpandedLines] = createSignal<Set<number>>(new Set())
  const [highlightLine, setHighlightLine] = createSignal<number | null>(null)
  const [initialScrollDone, setInitialScrollDone] = createSignal(false)
  const [searchInput, setSearchInput] = createSignal('')
  const [searchQuery, setSearchQuery] = createSignal('')
  const [allLoaded, setAllLoaded] = createSignal(false)
  const [loadingAll, setLoadingAll] = createSignal(false)

  let searchTimer: ReturnType<typeof setTimeout> | undefined

  function onSearchInput(value: string) {
    setSearchInput(value)
    clearTimeout(searchTimer)
    if (value === '') {
      setSearchQuery('')
    } else {
      searchTimer = setTimeout(() => setSearchQuery(value), 200)
    }
  }

  // Track in-flight fetches to avoid duplicates
  const fetchingRanges = new Set<string>()

  // Fetch session info
  const [sessionInfo] = createResource(
    () => params.id,
    async (id) => {
      const data = await query<{ sessionInfo: Session | null }>(
        SESSION_INFO_QUERY,
        { id },
      )
      return data.sessionInfo
    },
  )

  // Initial load: get total line count and first page
  const [initialLoad] = createResource(
    () => params.id,
    async (id) => {
      const data = await query<{
        sessionLogLines: { lines: LogLine[]; totalLines: number } | null
      }>(LOG_LINES_QUERY, { id, offset: 0, limit: PAGE_SIZE })

      if (!data.sessionLogLines) return null

      const { lines, totalLines: total } = data.sessionLogLines
      setTotalLines(total)

      const cache = new Map<number, string>()
      for (const l of lines) {
        cache.set(l.lineNumber, l.content)
      }
      setLineCache(cache)

      // If there's a uuid param, find which line it's on
      const targetUuid = searchParams.uuid
      if (targetUuid) {
        // Match the uuid field specifically, not parentUuid or messageId
        const needle = `"uuid":"${targetUuid}"`

        // Check if it's in the first page
        for (const l of lines) {
          if (l.content.includes(needle)) {
            setHighlightLine(l.lineNumber)
            setExpandedLines(new Set([l.lineNumber]))
            return { total }
          }
        }

        // Not in first page — fetch all remaining to find it
        if (total > PAGE_SIZE) {
          const rest = await query<{
            sessionLogLines: { lines: LogLine[]; totalLines: number } | null
          }>(LOG_LINES_QUERY, { id, offset: PAGE_SIZE, limit: total - PAGE_SIZE })

          if (rest.sessionLogLines) {
            const newCache = new Map(cache)
            for (const l of rest.sessionLogLines.lines) {
              newCache.set(l.lineNumber, l.content)
              if (l.content.includes(needle)) {
                setHighlightLine(l.lineNumber)
                setExpandedLines(new Set([l.lineNumber]))
              }
            }
            setLineCache(newCache)
          }
        }
      }

      return { total }
    },
  )

  async function fetchRange(start: number, end: number) {
    const key = `${start}-${end}`
    if (fetchingRanges.has(key)) return
    fetchingRanges.add(key)

    try {
      const data = await query<{
        sessionLogLines: { lines: LogLine[]; totalLines: number } | null
      }>(LOG_LINES_QUERY, {
        id: params.id,
        offset: start,
        limit: end - start,
      })

      if (data.sessionLogLines) {
        setLineCache((prev) => {
          const next = new Map(prev)
          for (const l of data.sessionLogLines!.lines) {
            next.set(l.lineNumber, l.content)
          }
          return next
        })
      }
    } finally {
      fetchingRanges.delete(key)
    }
  }

  // Fetch all lines for search filtering
  async function fetchAllLines() {
    if (allLoaded()) return
    setLoadingAll(true)
    try {
      const total = totalLines()
      const cache = lineCache()
      // Find missing ranges
      const promises: Promise<void>[] = []
      for (let offset = 0; offset < total; offset += PAGE_SIZE) {
        const end = Math.min(offset + PAGE_SIZE, total)
        let hasMissing = false
        for (let i = offset; i < end; i++) {
          if (!cache.has(i)) { hasMissing = true; break }
        }
        if (hasMissing) {
          promises.push(fetchRange(offset, end))
        }
      }
      await Promise.all(promises)
      setAllLoaded(true)
    } finally {
      setLoadingAll(false)
    }
  }

  // When search query becomes non-empty, ensure all lines are loaded
  createEffect(() => {
    if (searchQuery() && !allLoaded()) {
      fetchAllLines()
    }
  })

  // Compute filtered line indices
  const filteredIndices = createMemo<number[] | null>(() => {
    const q = searchQuery().toLowerCase()
    if (!q) return null // null = no filter active
    const cache = lineCache()
    const indices: number[] = []
    const total = totalLines()
    for (let i = 0; i < total; i++) {
      const content = cache.get(i)
      if (content && content.toLowerCase().includes(q)) {
        indices.push(i)
      }
    }
    return indices
  })

  const displayCount = () => {
    const fi = filteredIndices()
    return fi !== null ? fi.length : totalLines()
  }

  // Virtual scroll
  const virtualizer = createVirtualizer({
    get count() {
      return displayCount()
    },
    getScrollElement: () => scrollRef,
    estimateSize: () => 32,
    overscan: 20,
    measureElement: (el) => el.getBoundingClientRect().height,
  })

  // Scroll to target UUID after initial load
  createEffect(() => {
    const hl = highlightLine()
    if (hl !== null && !initialScrollDone() && totalLines() > 0) {
      setInitialScrollDone(true)
      // Defer to let virtualizer initialize
      requestAnimationFrame(() => {
        virtualizer.scrollToIndex(hl, { align: 'center' })
      })
    }
  })

  // Lazy load: when visible range approaches unfetched regions
  createEffect(() => {
    const items = virtualizer.getVirtualItems()
    if (items.length === 0) return

    const cache = lineCache()
    const start = items[0].index
    const end = items[items.length - 1].index

    // Check for gaps in the visible range + buffer
    const fetchStart = Math.max(0, start - 50)
    const fetchEnd = Math.min(totalLines(), end + 50)

    let gapStart: number | null = null
    for (let i = fetchStart; i < fetchEnd; i++) {
      if (!cache.has(i)) {
        if (gapStart === null) gapStart = i
      } else if (gapStart !== null) {
        // Align to page boundaries
        const pageStart = Math.floor(gapStart / PAGE_SIZE) * PAGE_SIZE
        const pageEnd = Math.min(
          pageStart + PAGE_SIZE,
          totalLines(),
        )
        fetchRange(pageStart, pageEnd)
        gapStart = null
      }
    }
    if (gapStart !== null) {
      const pageStart = Math.floor(gapStart / PAGE_SIZE) * PAGE_SIZE
      const pageEnd = Math.min(pageStart + PAGE_SIZE, totalLines())
      fetchRange(pageStart, pageEnd)
    }
  })

  function toggleLine(lineNum: number) {
    setExpandedLines((prev) => {
      const next = new Set(prev)
      if (next.has(lineNum)) {
        next.delete(lineNum)
      } else {
        next.add(lineNum)
      }
      return next
    })
  }

  // Download using the full raw log query
  async function download() {
    const data = await query<{ sessionRawLog: string | null }>(RAW_LOG_QUERY, {
      id: params.id,
    })
    const content = data.sessionRawLog
    if (!content) return
    const blob = new Blob([content], { type: 'application/jsonl' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = `${params.id}.jsonl`
    a.click()
    URL.revokeObjectURL(url)
  }

  return (
    <div class={styles['raw-log-view']}>
      <header>
        <A class={styles['back-link']} href={`/session/${params.id}`}>
          &larr; Back
        </A>
        <h1>Raw Log &mdash; {params.id.slice(0, 8)}</h1>
        <Show when={totalLines() > 0}>
          <div class={styles['search-box']}>
            <input
              class={styles['search-input']}
              type="text"
              placeholder="Filter lines..."
              value={searchInput()}
              onInput={(e) => onSearchInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === 'Escape') {
                  onSearchInput('')
                  e.currentTarget.blur()
                }
              }}
            />
            <Show when={searchQuery()}>
              <span class={styles['search-count']}>
                {loadingAll() ? 'loading...' : `${filteredIndices()?.length ?? 0} / ${totalLines()}`}
              </span>
            </Show>
          </div>
        </Show>
        <Show when={totalLines() > 0}>
          <span style={{ opacity: 0.5, 'font-size': '0.8rem' }}>
            {totalLines()} lines
          </span>
        </Show>
        <Show when={totalLines() > 0}>
          <button class={styles['download-btn']} onClick={download}>
            Download
          </button>
        </Show>
      </header>

      <Show when={sessionInfo()?.filePath}>
        {(fp) => <div class={styles['file-path']}>{fp()}</div>}
      </Show>

      <Show when={initialLoad.loading}>
        <p class={styles.status}>Loading...</p>
      </Show>
      <Show when={initialLoad.error}>
        <p class={`${styles.status} ${styles.error}`}>
          Error: {(initialLoad.error as Error).message}
        </p>
      </Show>
      <Show when={totalLines() === 0 && !initialLoad.loading && !initialLoad.error}>
        <p class={styles.status}>Empty log file.</p>
      </Show>

      <div ref={scrollRef} class={styles['virtual-scroll']}>
        <div
          class={styles['virtual-inner']}
          style={{ height: `${virtualizer.getTotalSize()}px` }}
        >
          <For each={virtualizer.getVirtualItems()}>
            {(vItem) => {
              const lineNum = () => {
                const fi = filteredIndices()
                return fi !== null ? fi[vItem.index] : vItem.index
              }
              const raw = () => lineCache().get(lineNum())
              const isExpanded = () => expandedLines().has(lineNum())
              const isHighlight = () => highlightLine() === lineNum()

              return (
                <Show
                  when={raw()}
                  fallback={
                    <div
                      data-index={vItem.index}
                      ref={(el) => queueMicrotask(() => virtualizer.measureElement(el))}
                      class={styles['line-row']}
                      style={{
                        transform: `translateY(${vItem.start}px)`,
                      }}
                    >
                      <span class={styles['line-num']}>{lineNum() + 1}</span>
                      <span style={{ opacity: 0.3 }}>Loading...</span>
                    </div>
                  }
                >
                  {(content) => {
                    const summary = () => parseSummary(content())

                    return (
                      <Show
                        when={isExpanded()}
                        fallback={
                          <div
                            data-index={vItem.index}
                            ref={(el) => queueMicrotask(() => virtualizer.measureElement(el))}
                            class={`${styles['line-row']} ${isHighlight() ? styles['highlight-line'] : ''}`}
                            style={{
                              transform: `translateY(${vItem.start}px)`,
                            }}
                            onClick={() => toggleLine(lineNum())}
                          >
                            <span class={styles['line-num']}>
                              {lineNum() + 1}
                            </span>
                            <span
                              class={`${styles['type-badge']} ${badgeClass(summary().type)}`}
                            >
                              {summary().type || '?'}
                            </span>
                            <span class={styles['line-uuid']}>
                              {summary().uuid
                                ? summary().uuid.slice(0, 8)
                                : ''}
                            </span>
                            <span class={styles['line-preview']}>
                              {content().length > 120
                                ? content().slice(0, 120) + '...'
                                : content()}
                            </span>
                            <span class={styles['line-timestamp']}>
                              {formatTimestamp(summary().timestamp)}
                            </span>
                          </div>
                        }
                      >
                        <div
                          data-index={vItem.index}
                          ref={(el) => queueMicrotask(() => virtualizer.measureElement(el))}
                          class={styles['line-expanded']}
                          style={{
                            transform: `translateY(${vItem.start}px)`,
                          }}
                        >
                          <div
                            class={`${styles['line-expanded-header']} ${isHighlight() ? styles['highlight-line'] : ''}`}
                            onClick={() => toggleLine(lineNum())}
                          >
                            <span class={styles['line-num']}>
                              {lineNum() + 1}
                            </span>
                            <span
                              class={`${styles['type-badge']} ${badgeClass(summary().type)}`}
                            >
                              {summary().type || '?'}
                            </span>
                            <span class={styles['line-uuid']}>
                              {summary().uuid
                                ? summary().uuid.slice(0, 8)
                                : ''}
                            </span>
                            <span class={styles['line-timestamp']}>
                              {formatTimestamp(summary().timestamp)}
                            </span>
                          </div>
                          <div class={styles['line-expanded-body']}>
                            <ExpandedJson raw={content()} />
                          </div>
                        </div>
                      </Show>
                    )
                  }}
                </Show>
              )
            }}
          </For>
        </div>
      </div>
    </div>
  )
}

function ExpandedJson(props: { raw: string }) {
  const parsed = () => {
    try {
      return JSON.parse(props.raw)
    } catch {
      return null
    }
  }

  return (
    <Show
      when={parsed()}
      fallback={<pre style={{ margin: 0, 'white-space': 'pre-wrap' }}>{props.raw}</pre>}
    >
      {(val) => <JsonTree value={val()} defaultExpandDepth={1} />}
    </Show>
  )
}
