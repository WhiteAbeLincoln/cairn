// Assistant response with model/token metadata and content blocks. Top-level DisplayItem kind='assistant'.

import { For, Show } from 'solid-js'
import type { DisplayableEvent, AssistantEvent, ToolResultEntry } from '../../lib/types'
import { totalTokens } from '../../lib/session'
import ContentBlockView from './ContentBlockView'
import MessageBlock from './MessageBlock'
import mb from './MessageBlock.module.css'
import styles from '../SessionView.module.css'

export default function AssistantMessageView(props: {
  msg: DisplayableEvent
  sessionId: string
  expanded: Set<string>
  toggle: (key: string) => void
  toolResults: Map<string, ToolResultEntry>
}) {
  const asst = () => props.msg as AssistantEvent
  return (
    <MessageBlock
      variant="assistant"
      role="assistant"
      label="Assistant"
      meta={{ sessionId: props.sessionId, uuid: props.msg.uuid, tokens: totalTokens(props.msg) }}
      extraMeta={
        <Show when={asst().message?.model}>
          {(m) => <span class={mb.model}>{m()}</span>}
        </Show>
      }
    >
      <div class={styles.blocks}>
        <For each={asst().message?.content ?? []}>
          {(block, idx) => (
            <ContentBlockView
              block={block}
              msg={props.msg}
              index={idx()}
              sessionId={props.sessionId}
              expanded={props.expanded}
              toggle={props.toggle}
              toolResults={props.toolResults}
            />
          )}
        </For>
      </div>
    </MessageBlock>
  )
}
