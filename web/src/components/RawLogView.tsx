import {
  createSignal,
  createMemo,
  createResource,
  createEffect,
  Show,
  For,
} from 'solid-js'
import { useParams, A } from '@solidjs/router'
import { createVirtualizer } from '@tanstack/solid-virtual'
import { query } from '../lib/graphql'
import { SessionPageQuery, SessionMetaQuery, RawLogQuery } from '../lib/queries'
import type { SessionMetaQuery as SessionMetaQueryType } from '../lib/generated/graphql'
import { JsonTree } from '../lib/json-tree'
import styles from './RawLogView.module.css'

const PAGE_SIZE = 200

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type RawEvent = Record<string, any>

type SessionInfo = NonNullable<SessionMetaQueryType['session']>['meta']

function getSummary(event: RawEvent): { type: string; uuid: string; timestamp: string } {
  return {
    type: (event?.type as string) ?? '',
    uuid: (event?.uuid as string) ?? '',
    timestamp: (event?.timestamp as string) ?? '',
  }
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
  const targetUuid = location.hash ? location.hash.slice(1) : undefined

  let scrollRef!: HTMLDivElement

  // Event cache: index -> parsed JSON event
  const [lineCache, setLineCache] = createSignal<Map<number, RawEvent>>(new Map())
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

  // Fetch session metadata
  const [sessionInfo] = createResource(
    () => params.id,
    async (id): Promise<SessionInfo | null> => {
      const data = await query(SessionMetaQuery, { id })
      return data.session?.meta ?? null
    },
  )

  // Initial load: get total event count and first page
  const [initialLoad] = createResource(
    () => params.id,
    async (id) => {
      const data = await query(SessionPageQuery, { id, page: { offset: 0, limit: PAGE_SIZE } })

      if (!data.session) return null

      const { events: wrapped, total } = data.session.events
      const events = wrapped.map(e => e.raw as RawEvent)
      setTotalLines(total)

      const cache = new Map<number, RawEvent>()
      for (let i = 0; i < events.length; i++) {
        cache.set(i, events[i])
      }
      setLineCache(cache)

      // If there's a uuid in the hash, find which event it's on
      if (targetUuid) {
        for (let i = 0; i < events.length; i++) {
          if (events[i].uuid === targetUuid) {
            setHighlightLine(i)
            setExpandedLines(new Set([i]))
            return { total }
          }
        }

        // Not in first page — fetch all remaining to find it
        if (total > PAGE_SIZE) {
          const rest = await query(SessionPageQuery, { id, page: { offset: PAGE_SIZE, limit: total - PAGE_SIZE } })

          if (rest.session) {
            const restEvents = rest.session.events.events.map(e => e.raw as RawEvent)
            const newCache = new Map(cache)
            for (let i = 0; i < restEvents.length; i++) {
              const idx = PAGE_SIZE + i
              newCache.set(idx, restEvents[i])
              if (restEvents[i].uuid === targetUuid) {
                setHighlightLine(idx)
                setExpandedLines(new Set([idx]))
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
      const data = await query(SessionPageQuery, {
        id: params.id,
        page: { offset: start, limit: end - start },
      })

      if (data.session) {
        const fetched = data.session.events.events.map(e => e.raw as RawEvent)
        setLineCache((prev) => {
          const next = new Map(prev)
          for (let i = 0; i < fetched.length; i++) {
            next.set(start + i, fetched[i])
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

  // Compute filtered event indices
  const filteredIndices = createMemo<number[] | null>(() => {
    const q = searchQuery().toLowerCase()
    if (!q) return null // null = no filter active
    const cache = lineCache()
    const indices: number[] = []
    const total = totalLines()
    for (let i = 0; i < total; i++) {
      const event = cache.get(i)
      if (event && JSON.stringify(event).toLowerCase().includes(q)) {
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
    const data = await query(RawLogQuery, { id: params.id })
    const content = data.session?.rawLog ?? null
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
                  {(event) => {
                    const summary = () => getSummary(event())
                    const preview = () => {
                      const s = JSON.stringify(event())
                      return s.length > 120 ? s.slice(0, 120) + '...' : s
                    }

                    return (
                      <Show
                        when={isExpanded()}
                        fallback={
                          <div
                            id={summary().uuid || undefined}
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
                              {preview()}
                            </span>
                            <span class={styles['line-timestamp']}>
                              {formatTimestamp(summary().timestamp)}
                            </span>
                          </div>
                        }
                      >
                        <div
                          id={summary().uuid || undefined}
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
                            <JsonTree value={event()} defaultExpandDepth={1} />
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
