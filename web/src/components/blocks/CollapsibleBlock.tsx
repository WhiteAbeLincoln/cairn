import type { JSX, ParentProps } from 'solid-js'
import { Show } from 'solid-js'
import { A } from '@solidjs/router'
import styles from '../SessionView.module.css'

export default function CollapsibleBlock(props: ParentProps<{
  expanded: boolean
  toggle: () => void
  role: string
  sessionId: string
  uuid: string
  class?: string
  classList?: Record<string, boolean | undefined>
  label: JSX.Element
  tokens?: number
}>) {
  return (
    <div id={props.uuid} class={props.class} classList={{ ...props.classList, [styles['is-expanded']]: props.expanded }} data-role={props.role}>
      <button class={styles['internal-toggle']} onClick={() => props.toggle()}>
        <a class={styles['link-icon']} href={`#${props.uuid}`} onClick={(e) => e.stopPropagation()} title="Link to this block">
          &#x1F517;
        </a>
        <span class={styles.caret}>
          {props.expanded ? '\u25BE' : '\u25B8'}
        </span>
        <span class={styles['internal-steps']}>
          {props.label}
        </span>
        <Show when={props.tokens != null && props.tokens! > 0}>
          <span class={styles['internal-tokens']}>
            {props.tokens!.toLocaleString()} tok
          </span>
        </Show>
        <A class={styles.uuid} href={`/session/${props.sessionId}/raw#${props.uuid}`}>
          {props.uuid.slice(0, 8)}
        </A>
      </button>
      <Show when={props.expanded}>
        {props.children}
      </Show>
    </div>
  )
}
