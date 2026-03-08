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
  extraLabel?: JSX.Element
  event?: DisplayItem
  extraMeta?: JSX.Element
  role?: string

  id?: string
  class?: string
  classList?: Record<string, boolean | undefined>
  style?: JSX.DOMAttributes<HTMLDivElement>['style']

  isRaw?: boolean
  onToggleRaw?: () => void
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

  const nameContent = () => (
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
  )

  const middleContent = () => (
    <span class={mb['header-middle']}>
      {props.extraLabel}
      {props.extraMeta}
      <span class={mb['header-spacer']} />
      <Show when={props.event}>
        {(e) => <MessageMeta event={e()} />}
      </Show>
    </span>
  )

  const fixedEndContent = () => (
    <>
      <Show when={props.event}>
        {(e) => <ErrorBadge event={e()} />}
      </Show>
      <Show when={props.onToggleRaw}>
        {(toggle) => (
          <button
            class={mb['raw-toggle']}
            classList={{ [mb['raw-toggle-active']]: props.isRaw }}
            onClick={(e) => {
              e.stopPropagation()
              toggle()()
            }}
          >
            {'{}'}
          </button>
        )}
      </Show>
    </>
  )

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
      <Switch>
        <Match when={props.kind === 'collapsed' || props.kind === 'grouped'}>
          <div class={cb['toggle']} role="button" tabindex="0" onClick={() => props.onExpand?.()}>
            <span class={cb.caret}>
              {props.expanded ? '\u25BE' : '\u25B8'}
            </span>
            {nameContent()}
            {middleContent()}
            {fixedEndContent()}
          </div>
        </Match>
        <Match when={props.kind === 'full'}>
          <div class={mb['header']}>
            {nameContent()}
            {middleContent()}
            {fixedEndContent()}
          </div>
        </Match>
      </Switch>
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

function useToolResult(event: () => DisplayItem) {
  const ctx = useContext(SessionContext)
  return createMemo(() => {
    const e = event()
    if (e.kind === 'tool-result') return e
    if (e.kind === 'tool-use') return ctx.getToolResult(e.content.id)
    return undefined
  })
}

function ErrorBadge(props: { event: DisplayItem }) {
  const toolResult = useToolResult(() => props.event)
  const isError = () =>
    !!(toolResult()?.content as { is_error?: unknown } | undefined)?.is_error

  return (
    <Show when={isError()}>
      <span class={styles['error-badge']} style={{ color: 'white', 'flex-shrink': '0' }}>
        error
      </span>
    </Show>
  )
}

function MessageMeta(props: { event: DisplayItem }) {
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
    <>
      <Show when={messageInfo()?.model}>{(m) => <span>{m()}</span>}</Show>
      <Show when={messageInfo()?.model && messageInfo()?.tokens}>
        <span class={cb['step-dot']}>&middot;</span>
      </Show>
      <Show when={messageInfo()?.tokens}>
        {(t) => <span class={mb['tokens']}>{t().toLocaleString()} tok</span>}
      </Show>
    </>
  )
}
