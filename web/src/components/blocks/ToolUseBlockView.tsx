// Collapsible tool use block with input/output sections.
// Used inside ContentBlockView (from AssistantMessageView and InternalGroupView).

import { Show } from 'solid-js'
import { formatInput, truncate } from '../../lib/format'
import { contentToString } from '../../lib/session'
import type { ToolResultEntry } from '../../lib/types'
import CollapsibleBlock from './CollapsibleBlock'
import styles from '../SessionView.module.css'

export default function ToolUseBlockView(props: {
  blockKey: string
  name: string
  input: unknown
  result: ToolResultEntry | undefined
  sessionId: string
  uuid: string
  expanded: Set<string>
  toggle: (key: string) => void
  tokens?: number
}) {
  return (
    <CollapsibleBlock
      role="tool"
      meta={{ sessionId: props.sessionId, uuid: props.uuid, tokens: props.tokens, result: props.result }}
      class={styles['tool-block']}
      expanded={props.expanded.has(props.blockKey)}
      toggle={() => props.toggle(props.blockKey)}
      label={<span class={styles.step}>{props.name}</span>}
    >
      <div class={styles['tool-details']}>
        <div class={styles['tool-section']}>
          <div class={styles['tool-section-label']}>Input</div>
          <pre>{formatInput(props.input)}</pre>
        </div>
        <Show when={props.result}>
          {(r) => (
            <div class={styles['tool-section']}>
              <div class={styles['tool-section-label']}>Output</div>
              <pre classList={{ [styles['is-error']]: !!r().is_error }}>
                {truncate(contentToString(r().content), 5000)}
              </pre>
            </div>
          )}
        </Show>
      </div>
    </CollapsibleBlock>
  )
}
