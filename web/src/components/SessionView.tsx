import {
  createSignal,
  createResource,
  createMemo,
  createEffect,
  For,
  Switch,
  Match,
  onCleanup,
  type Accessor,
  type Setter,
} from 'solid-js'
import { useParams, A } from '@solidjs/router'
import { query, subscribe } from '../lib/graphql'
import { SessionQuery, SessionEventsSubscription } from '../lib/queries'
import { mkToggle } from '../lib/types'
import styles from './SessionView.module.css'
import { type RawEvent } from './RawEventRow'
import { ReactiveMap } from '@solid-primitives/map'
import {
  eventsToDisplayItems,
  type DisplayItemWithMode,
  type ToolResultMap,
  type ToolUseMap,
} from '../lib/display-item'
import { ReactiveSet } from '@solid-primitives/set'
import { SessionContext, type SessionContextValue } from './session-context'
import { DisplayItemView } from './blocks/DisplayItemView'

// --- Main component ---

export default function SessionView() {
  const params = useParams<{ id: string }>()

  const [sessionData] = createResource(
    () => params.id,
    async (id) => {
      const data = await query(SessionQuery, { id })
      return data.session
    },
  )

  const {
    live,
    setLive,
    liveEvents: rawLiveEvents,
  } = liveSubscription(() => params.id)
  const [raw, setRaw] = createSignal(false)
  const expandToggle = new ReactiveSet<string>()
  const rawOverrideToggle = mkToggle<string>()
  const toolUseMap: ToolUseMap = new ReactiveMap()
  const toolResultMap: ToolResultMap = new ReactiveMap()
  const toggle = (set: ReactiveSet<string>) => (key: string) =>
    set.has(key) ? set.delete(key) : set.add(key)
  const sessionState: SessionContextValue = {
    isExpanded: (key) => expandToggle.has(key),
    toggleExpanded: toggle(expandToggle),

    globalRaw: raw,
    displayAsRaw: (key) => rawOverrideToggle.toggled(key),
    toggleRawDisplay: (key) => rawOverrideToggle.toggle(key),

    getToolUse: (key) => toolUseMap.get(key),
    getToolResult: (key) => toolResultMap.get(key),
  }

  const baseMessages = createMemo(() => {
    const data = Iterator.from(sessionData()?.events.events ?? []).map(
      (e) => e.raw as RawEvent,
    )
    return [...eventsToDisplayItems(data, toolUseMap, toolResultMap)]
  })

  const messages = createMemo((): DisplayItemWithMode[] => {
    const base = baseMessages()
    const live = [
      ...eventsToDisplayItems(
        rawLiveEvents(),
        toolUseMap,
        toolResultMap,
        base.length,
      ),
    ]
    // if last base is grouped and first live is grouped, merge them
    if (base.length > 0 && live.length > 0) {
      const lastBase = base[base.length - 1]
      const firstLive = live[0]
      if (lastBase.mode === 'grouped' && firstLive.mode === 'grouped') {
        return [
          ...base.slice(0, -1),
          { items: [...lastBase.items, ...firstLive.items], mode: 'grouped' },
          ...live.slice(1),
        ]
      }
    }
    const ret = [...base, ...live]
    return ret
  })

  return (
    <SessionContext.Provider value={sessionState}>
      <div class={styles['session-view']}>
        <SessionHeader
          sessionId={params.id}
          live={live()}
          setLive={setLive}
          raw={raw()}
          setRaw={setRaw}
        />

        <Switch>
          <Match when={sessionData.loading}>
            <p class={styles.status}>Loading session...</p>
          </Match>
          <Match when={sessionData.error}>
            <p class={`${styles.status} ${styles.error}`}>
              Error: {(sessionData.error as Error).message}
            </p>
          </Match>
          <Match when={true}>
            <div class={styles.messages}>
              <For each={messages()}>
                {(msg, idx) => <DisplayItemView event={msg} idx={idx()} />}
              </For>
            </div>
          </Match>
        </Switch>
      </div>
    </SessionContext.Provider>
  )
}

function liveSubscription(id: Accessor<string>): {
  live: Accessor<boolean>
  setLive: Setter<boolean>
  liveEvents: Accessor<RawEvent[]>
} {
  const scrollContainer = () => document.querySelector('main')

  const [live, setLive] = createSignal(false)
  const [liveEvents, setLiveEvents] = createSignal<RawEvent[]>([])
  createEffect(() => {
    if (!live()) return

    const unsub = subscribe(SessionEventsSubscription, { id: id() }, (data) => {
      setLiveEvents((prev) => [...prev, data.sessionEvents.raw as RawEvent])
    })

    onCleanup(unsub)
  })

  // Auto-scroll to bottom when new events arrive in live mode
  createEffect(() => {
    void liveEvents().length
    const el = scrollContainer()
    if (!live() || !el) return
    requestAnimationFrame(() => {
      el.scrollTop = el.scrollHeight
    })
  })

  // Reset live events when toggling off
  createEffect(() => {
    if (!live()) setLiveEvents([])
  })

  return { live, setLive, liveEvents }
}

function SessionHeader(props: {
  sessionId: string
  live: boolean
  setLive: (v: boolean | ((prev: boolean) => boolean)) => void
  raw: boolean
  setRaw: (v: boolean | ((prev: boolean) => boolean)) => void
}) {
  return (
    <header class={styles['sticky-header']}>
      <A class={styles['back-link']} href="/">
        &larr; Back
      </A>
      <h1>Session {props.sessionId.slice(0, 8)}</h1>
      <div class={styles['header-spacer']} />
      <button
        class={`${styles['live-toggle']} ${props.live ? styles['live-active'] : ''}`}
        onClick={() => props.setLive((v) => !v)}
      >
        Live
      </button>
      <button
        class={`${styles['live-toggle']} ${props.raw ? styles['live-active'] : ''}`}
        onClick={() => props.setRaw((v) => !v)}
      >
        Raw
      </button>
    </header>
  )
}
