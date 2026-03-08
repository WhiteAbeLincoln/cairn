import {
  type JSX,
  createMemo,
  useContext,
  Switch,
  Match,
  For,
  Show,
} from 'solid-js'
import { Dynamic } from 'solid-js/web'
import type { DisplayItemWithMode, DisplayItem } from '../../lib/display-item'
import { JsonTree } from '../../lib/json-tree'
import { upperFirst } from '../../lib/util'
import { SessionContext } from '../session-context'
import Prose from '../Prose'
import MessageBlock from './MessageBlock'
import ToolUseBlockView from './ToolUseBlockView'
import mb from './MessageBlock.module.css'
import tb from './ThinkingBlockView.module.css'
import styles from '../SessionView.module.css'
import rer from '../RawEventRow.module.css'
import RawEventRow from '../RawEventRow'

export function DisplayItemView(props: {
  event: DisplayItemWithMode
  idx: number
}): JSX.Element {
  const displayEvents = createMemo(() =>
    props.event.mode === 'grouped' ? props.event.items : [props.event.item],
  )
  const ctx = useContext(SessionContext)
  return (
    <Switch>
      <Match when={ctx.globalRaw()}>
        <For each={displayEvents()}>
          {(evt) => (
            <>
              <RawDisplayItem event={evt} />
              <span></span>
            </>
          )}
        </For>
      </Match>
      <Match when={props.event.mode === 'hidden'}>{null}</Match>
      <Match
        when={
          props.event.mode === 'grouped' && props.event.items.length > 1
            ? props.event
            : undefined
        }
      >
        {(e) => (
          <>
            <GroupedEvent events={e().items} />
            <span></span>
          </>
        )}
      </Match>
      <Match when={true}>
        <RenderDisplayItem
          event={
            props.event as Exclude<DisplayItemWithMode, { mode: 'hidden' }>
          }
        />
      </Match>
    </Switch>
  )
}

function GroupedEvent(props: { events: DisplayItem[] }) {
  const ctx = useContext(SessionContext)
  const id = () => {
    const first = props.events[0]
    const last = props.events[props.events.length - 1]
    return `group-${first.id}-${last.id}`
  }

  const steps = createMemo(() => {
    const stepMap = new Map<string, number>()

    for (const evt of props.events) {
      const name = evtToName(evt)
      stepMap.set(name, (stepMap.get(name) ?? 0) + 1)
    }
    return [...stepMap.entries()]
  })

  return (
    <MessageBlock
      kind="grouped"
      id={id()}
      expanded={ctx.isExpanded(id())}
      onExpand={() => ctx.toggleExpanded(id())}
      steps={steps()}
      label={null}
    >
      <For each={props.events}>
        {(evt) => (
          <RenderDisplayItem event={{ item: evt, mode: 'collapsed' }} />
        )}
      </For>
    </MessageBlock>
  )
}

function RenderDisplayItem(props: {
  event: Exclude<DisplayItemWithMode, { mode: 'hidden' }>
}) {
  // we already handle the case where a grouped event has multiple items, so if it's grouped it must have exactly 1 item
  const displayItem = () =>
    props.event.mode === 'grouped' ? props.event.items[0] : props.event.item
  const id = () => displayItem().id
  const rawItem = () => displayItem().event
  const displayMode = () =>
    props.event.mode === 'grouped' ? 'collapsed' : props.event.mode
  const ctx = useContext(SessionContext)

  return (
    <>
      {/* special case for turn duration which we don't want to wrap */}
      <Show
        when={
          !(displayItem().kind == 'turn-duration' && displayMode() === 'full')
        }
        fallback={<TurnDuration event={displayItem() as any} />}
      >
        {(_) => (
          <MessageBlock
            kind={displayMode()}
            label={evtToName(displayItem())}
            expanded={ctx.isExpanded(id())}
            onExpand={() => ctx.toggleExpanded(id())}
            id={displayItem().id}
            event={displayItem()}
          >
            <Dynamic
              component={eventRenderMap[displayItem().kind]}
              event={displayItem() as any}
            />
          </MessageBlock>
        )}
      </Show>
      <button
        class={styles['inspect-message-icon']}
        classList={{
          [styles['inspect-message-icon-toggled']]: ctx.displayAsRaw(id()),
        }}
        onClick={() => ctx.toggleRawDisplay(id())}
      >
        {'{}'}
      </button>
      <Show when={ctx.displayAsRaw(id())}>
        <div class={rer['line-expanded-body']}>
          <JsonTree value={rawItem()} defaultExpandDepth={1} />
        </div>
        {/* we need an empty element or else the grid layout breaks */}
        <span></span>
      </Show>
    </>
  )
}

function TurnDuration(props: {
  event: Extract<DisplayItem, { kind: 'turn-duration' }>
}) {
  return (
    <div class={`${mb.message} ${mb.system}`} data-role="system">
      Turn completed in {(props.event.event.durationMs! / 1000).toFixed(1)}s
    </div>
  )
}

type EventRenderMap = {
  [k in DisplayItem['kind']]: (props: {
    event: Extract<DisplayItem, { kind: k }>
  }) => JSX.Element
}

const eventRenderMap: EventRenderMap = {
  'user-message': (props) => <Prose text={props.event.content} />,
  'assistant-message': (props) => <Prose text={props.event.content.text} />,
  compaction: (props) => <Prose text={props.event.content} />,
  thinking: (props) => (
    <Prose
      text={props.event.content.thinking}
      class={`${tb['thinking-content']} ${tb['prose-mono']}`}
    />
  ),
  'tool-use': ToolUseBlockView,
  'tool-result': ToolUseBlockView,
  'turn-duration': TurnDuration,
  other: RawDisplayItem,
}

function evtToName(evt: DisplayItem): string {
  switch (evt.kind) {
    case 'user-message':
      return 'User'
    case 'compaction':
      return 'Compaction'
    case 'thinking':
      return 'Thinking'
    case 'tool-result':
      return 'Tool Result'
    case 'tool-use':
      return evt.content.name
    case 'assistant-message':
      return 'Assistant'
    case 'turn-duration':
      return 'Turn Duration'
    case 'other':
      const sub = evt.event.subtype ? ` (${evt.event.subtype})` : ''
      return `${upperFirst(evt.event.type)} Event${sub}`
  }
}

function RawDisplayItem(props: { event: DisplayItem }) {
  const ctx = useContext(SessionContext)

  return (
    <RawEventRow
      event={props.event.event}
      expanded={ctx.isExpanded(props.event.id)}
      onToggle={() => ctx.toggleExpanded(props.event.id)}
    />
  )
}
