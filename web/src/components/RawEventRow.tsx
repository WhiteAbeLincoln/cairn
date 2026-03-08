import { Show } from 'solid-js'
import type { JSX } from 'solid-js'
import { JsonTree } from '../lib/json-tree'
import styles from './RawEventRow.module.css'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type RawEvent = Record<string, any>

function getSummary(event: RawEvent): {
  type: string
  uuid: string
  timestamp: string
} {
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
    return new Date(ts).toLocaleTimeString()
  } catch {
    return ts
  }
}

export interface RawEventRowProps {
  event: RawEvent
  expanded: boolean
  onToggle?: () => void
  highlighted?: boolean
  lineNum?: number
  id?: string
  ref?: (el: HTMLDivElement) => void
  style?: JSX.CSSProperties
  class?: string
  'data-index'?: number
}

export default function RawEventRow(props: RawEventRowProps) {
  const summary = () => getSummary(props.event)
  const preview = () => {
    const s = JSON.stringify(props.event)
    return s.length > 120 ? s.slice(0, 120) + '...' : s
  }

  return (
    <Show
      when={props.expanded}
      fallback={
        <div
          id={props.id ?? (summary().uuid || undefined)}
          data-index={props['data-index']}
          ref={props.ref}
          class={`${styles['line-row']} ${props['class'] ?? ''} ${props.highlighted ? styles['highlight-line'] : ''}`}
          style={props.style}
          onClick={() => props.onToggle?.()}
        >
          <Show when={props.lineNum != null}>
            <span class={styles['line-num']}>{props.lineNum! + 1}</span>
          </Show>
          <span class={`${styles['type-badge']} ${badgeClass(summary().type)}`}>
            {summary().type || '?'}
          </span>
          <span class={styles['line-uuid']}>
            {summary().uuid ? summary().uuid.slice(0, 8) : ''}
          </span>
          <span class={styles['line-preview']}>{preview()}</span>
          <span class={styles['line-timestamp']}>
            {formatTimestamp(summary().timestamp)}
          </span>
        </div>
      }
    >
      <div
        id={props.id ?? (summary().uuid || undefined)}
        data-index={props['data-index']}
        ref={props.ref}
        class={`${styles['line-expanded']} ${props['class'] ?? ''}`}
        style={props.style}
      >
        <div
          class={`${styles['line-expanded-header']} ${props.highlighted ? styles['highlight-line'] : ''}`}
          onClick={() => props.onToggle?.()}
        >
          <Show when={props.lineNum != null}>
            <span class={styles['line-num']}>{props.lineNum! + 1}</span>
          </Show>
          <span class={`${styles['type-badge']} ${badgeClass(summary().type)}`}>
            {summary().type || '?'}
          </span>
          <span class={styles['line-uuid']}>
            {summary().uuid ? summary().uuid.slice(0, 8) : ''}
          </span>
          <span class={styles['line-timestamp']}>
            {formatTimestamp(summary().timestamp)}
          </span>
        </div>
        <div class={styles['line-expanded-body']}>
          <JsonTree value={props.event} defaultExpandDepth={1} />
        </div>
      </div>
    </Show>
  )
}
