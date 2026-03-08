import {
  createMemo,
  For,
  Match,
  Show,
  Switch,
  useContext,
  type JSX,
  type ParentProps,
} from 'solid-js'
import mb from './MessageBlock.module.css'
import cb from './CollapsibleBlock.module.css'
import styles from '../SessionView.module.css'
import type { DisplayItem } from '../../lib/display-item'
import { SessionContext } from '../session-context'

type MessageBlockPropsBase = {
  label: JSX.Element
  event?: DisplayItem
  extraMeta?: JSX.Element
  role?: string

  id?: string
  class?: string
  classList?: Record<string, boolean | undefined>
  style?: JSX.DOMAttributes<HTMLDivElement>['style']
}
type MessageBlockProps = (
  | { kind: 'collapsed'; expanded: boolean; onExpand: () => void }
  | {
      kind: 'grouped'
      expanded: boolean
      onExpand: () => void
      steps: [string, number][]
    }
  | { kind: 'full'; expanded?: boolean; onExpand?: () => void }
) &
  MessageBlockPropsBase

export default function MessageBlock(props: ParentProps<MessageBlockProps>) {
  const role = () =>
    props.kind === 'grouped'
      ? 'group'
      : props.role || props.event?.kind || undefined
  const toolName = () =>
    props.event?.kind === 'tool-use' ? props.event.content.name : undefined
  return (
    <div
      id={props.id}
      class={props.class}
      classList={{
        ...props.classList,
        [mb[props.kind]]: true,
      }}
      data-expanded={props.expanded || undefined}
      data-role={role()}
      data-tool-name={toolName()}
      style={props.style}
    >
      <MessageBlockHeader
        kind={props.kind}
        expanded={props.expanded}
        onExpand={props.onExpand}
      >
        <span class={cb['label']}>
          <Show when={props.kind === 'grouped' ? props.steps : undefined}>
            {(steps) => (
              <For each={steps()}>
                {([name, count], si) => (
                  <>
                    <Show when={si() > 0}>
                      <span class={cb['step-dot']}>&middot;</span>
                    </Show>
                    <span class={styles.step}>
                      {name}
                      <Show when={count > 1}> &times;{count}</Show>
                    </span>
                  </>
                )}
              </For>
            )}
          </Show>
          {props.label}
        </span>
        {props.extraMeta}
        <Show when={props.event}>{(e) => <MessageMeta event={e()} />}</Show>
      </MessageBlockHeader>
      <Switch>
        <Match
          when={
            (props.expanded && props.kind !== 'grouped') ||
            props.kind === 'full'
          }
        >
          {props.children}
        </Match>
        <Match when={props.expanded && props.kind === 'grouped'}>
          <div class={mb['grouped-items']}>{props.children}</div>
        </Match>
      </Switch>
    </div>
  )
}

function MessageBlockHeader(
  props: ParentProps<{
    kind: 'collapsed' | 'full' | 'grouped'
    onExpand?: () => void
    expanded?: boolean
  }>,
) {
  return (
    <Switch>
      <Match when={props.kind === 'collapsed' || props.kind === 'grouped'}>
        <button class={cb['toggle']} onClick={() => props.onExpand?.()}>
          {/*<a class={cb['link-icon']} href={`#${blockId()}`} onClick={(e) => e.stopPropagation()} title="Link to this block">
            &#x1F517;
          </a>*/}
          <span class={cb.caret}>{props.expanded ? '\u25BE' : '\u25B8'}</span>
          {props.children}
        </button>
      </Match>
      <Match when={props.kind === 'full'}>
        <div class={mb['header']}>{props.children}</div>
      </Match>
    </Switch>
  )
}

export type MetaProps = {
  event: DisplayItem
}

function MessageMeta(props: MetaProps) {
  const ctx = useContext(SessionContext)

  const toolResult = createMemo(() => {
    if (props.event.kind === 'tool-result') {
      return props.event
    }

    if (props.event.kind === 'tool-use') {
      const result = ctx.getToolResult(props.event.content.id)
      return result
    }

    return undefined
  })

  const isError = () =>
    !!(toolResult()?.content as { is_error?: unknown } | undefined)?.is_error

  // tokens is easier, it appears on almost all events
  const messageInfo = createMemo(
    (): { tokens?: number; model?: string } | undefined => {
      const msg = props.event.event.message
      if (typeof msg !== 'object' || !msg) {
        return undefined
      }

      const ret: { tokens?: number; model?: string } = {}

      if (typeof msg?.usage === 'object' && msg?.usage) {
        const usage = msg.usage
        ret.tokens = (usage.input_tokens ?? 0) + (usage.output_tokens ?? 0)
      }

      if (msg.model) {
        ret.model = msg.model
      }

      return ret
    },
  )

  return (
    <span class={mb.meta}>
      <Show when={isError()}>
        <span class={styles['error-badge']} style={{ color: 'white' }}>
          error
        </span>
      </Show>
      <Show when={messageInfo()?.model}>{(m) => <span>{m()}</span>}</Show>
      <Show when={messageInfo()?.model && messageInfo()?.tokens}>
        <span class={cb['step-dot']}>&middot;</span>
      </Show>
      <Show when={messageInfo()?.tokens}>
        {(t) => <span class={mb['tokens']}>{t().toLocaleString()} tok</span>}
      </Show>
    </span>
  )
}
