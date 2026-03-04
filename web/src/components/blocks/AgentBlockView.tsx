// Agent tool call rendered as a collapsible block with link to the subagent session.
// Top-level DisplayItem kind='agent'.

import { Show } from 'solid-js'
import { A } from '@solidjs/router'
import { truncate } from '../../lib/format'
import type { DisplayableEvent, ToolResultEntry } from '../../lib/types'
import { getAgentBlock, totalTokens, contentToString } from '../../lib/session'
import CollapsibleBlock from './CollapsibleBlock'
import cb from './CollapsibleBlock.module.css'
import ab from './AgentBlockView.module.css'
import styles from '../SessionView.module.css'

export default function AgentBlockView(props: {
  msg: DisplayableEvent
  sessionId: string
  toolResults: Map<string, ToolResultEntry>
  agentMap: Map<string, string>
  expanded: Set<string>
  toggle: (key: string) => void
}) {
  const block = getAgentBlock(props.msg)!
  const input = block.input as {
    description?: string
    prompt?: string
    subagent_type?: string
  }
  const description = input.description ?? ''
  const subagentType = input.subagent_type ?? ''
  const result = props.toolResults.get(block.id)
  const agentId = () => props.agentMap.get(block.id)
  const key = `${props.msg.uuid}-agent`
  const outputKey = `${props.msg.uuid}-agent-output`
  return (
    <CollapsibleBlock
      role="agent"
      meta={{ sessionId: props.sessionId, uuid: props.msg.uuid, tokens: totalTokens(props.msg) ?? undefined }}
      class={cb['internal-single']}
      classList={{
        [styles['tool-block']]: true,
      }}
      expanded={props.expanded.has(key)}
      toggle={() => props.toggle(key)}
      label={
        <>
          <span class={styles.step}>Agent</span>
          <Show when={subagentType}>
            <span class={cb['step-dot']}>&middot;</span>
            <span class={styles.step}>{subagentType}</span>
          </Show>
          <span class={cb['step-dot']}>&middot;</span>
          <span class={styles.step}>{description}</span>
        </>
      }
    >
      <div class={ab['agent-expanded']}>
        <Show when={agentId()}>
          {(aid) => (
            <A
              class={ab['agent-link']}
              href={`/session/agent-${aid()}`}
            >
              View subagent session &rarr;
            </A>
          )}
        </Show>
        <Show when={result}>
          {(r) => (
            <div class={ab['agent-output-section']}>
              <button class={styles.toggle} onClick={() => props.toggle(outputKey)}>
                {props.expanded.has(outputKey) ? '\u25BE' : '\u25B8'} Output
              </button>
              <Show when={props.expanded.has(outputKey)}>
                <pre class={ab['agent-output']}>
                  {truncate(contentToString(r().content), 5000)}
                </pre>
              </Show>
            </div>
          )}
        </Show>
      </div>
    </CollapsibleBlock>
  )
}
