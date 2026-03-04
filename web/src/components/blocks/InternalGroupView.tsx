// Non-user-facing assistant messages (thinking, tool calls) grouped into collapsible blocks.
// Single-step groups render the block directly; multi-step groups collapse behind a step summary.
// Top-level DisplayItem kind='internal-group'.

import { For, Show } from 'solid-js'
import type { DisplayableEvent, AssistantEvent, ToolResultEntry } from '../../lib/types'
import { compactSteps } from '../../lib/session'
import ContentBlockView from './ContentBlockView'
import CollapsibleBlock from './CollapsibleBlock'
import cb from './CollapsibleBlock.module.css'
import styles from '../SessionView.module.css'

export default function InternalGroupView(props: {
  groupKey: string
  steps: string[]
  tokens: number
  msgs: DisplayableEvent[]
  sessionId: string
  expanded: Set<string>
  toggle: (key: string) => void
  toolResults: Map<string, ToolResultEntry>
}) {
  const blocks = (msg: DisplayableEvent) =>
    msg.type === 'assistant' ? (msg as AssistantEvent).message.content : []

  if (props.steps.length === 1) {
    return (
      <For each={props.msgs}>
        {(msg) => (
          <For each={blocks(msg)}>
            {(block, idx) => (
              <ContentBlockView
                block={block}
                msg={msg}
                index={idx()}
                sessionId={props.sessionId}
                expanded={props.expanded}
                toggle={props.toggle}
                toolResults={props.toolResults}
                tokens={props.tokens}
              />
            )}
          </For>
        )}
      </For>
    )
  }

  return (
    <CollapsibleBlock
      role="internal-group"
      meta={{ sessionId: props.sessionId, uuid: props.msgs[0].uuid, tokens: props.tokens }}
      id={props.groupKey}
      class={cb['internal-group']}
      expanded={props.expanded.has(props.groupKey)}
      toggle={() => props.toggle(props.groupKey)}
      label={
        <For each={compactSteps(props.steps)}>
          {(step, si) => (
            <>
              <Show when={si() > 0}>
                <span class={cb['step-dot']}>&middot;</span>
              </Show>
              <span class={styles.step}>
                {step.name}
                <Show when={step.count > 1}>
                  {' '}
                  &times;{step.count}
                </Show>
              </span>
            </>
          )}
        </For>
      }
    >
      <div class={cb['internal-expanded']}>
        <For each={props.msgs}>
          {(msg) => (
            <For each={blocks(msg)}>
              {(block, idx) => (
                <ContentBlockView
                  block={block}
                  msg={msg}
                  index={idx()}
                  sessionId={props.sessionId}
                  expanded={props.expanded}
                  toggle={props.toggle}
                  toolResults={props.toolResults}
                />
              )}
            </For>
          )}
        </For>
      </div>
    </CollapsibleBlock>
  )
}
