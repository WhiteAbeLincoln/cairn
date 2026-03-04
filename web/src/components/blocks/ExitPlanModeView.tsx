// ExitPlanMode tool call rendered as a markdown plan with collapsible accepted/rejected output.
// Top-level DisplayItem kind='exit-plan-mode'.

import { Show } from 'solid-js'
import type { DisplayableEvent, ToolResultEntry } from '../../lib/types'
import { getToolUseBlock, totalTokens, contentToString } from '../../lib/session'
import MessageBlock from './MessageBlock'
import Prose from '../Prose'
import ep from './ExitPlanModeView.module.css'
import styles from '../SessionView.module.css'

export default function ExitPlanModeView(props: {
  msg: DisplayableEvent
  sessionId: string
  toolResults: Map<string, ToolResultEntry>
  expanded: Set<string>
  toggle: (key: string) => void
}) {
  const block = getToolUseBlock(props.msg, 'ExitPlanMode')!
  const plan = (block.input as { plan?: string }).plan ?? ''
  const result = props.toolResults.get(block.id)
  const outputKey = `${props.msg.uuid}-plan-output`
  return (
    <MessageBlock
      variant="exit-plan-mode"
      role="exit-plan-mode"
      label="Plan"
      meta={{ sessionId: props.sessionId, uuid: props.msg.uuid, tokens: totalTokens(props.msg) }}
    >
      <Prose
        text={plan}
        class={ep['plan-content']}
      />
      <Show when={result}>
        {(r) => {
          const text = () => contentToString(r().content)
          return (
            <div class={ep['plan-output']}>
              <button class={styles.toggle} onClick={() => props.toggle(outputKey)}>
                {props.expanded.has(outputKey) ? '\u25BE' : '\u25B8'} Output
                <Show when={text().includes('rejected')}>
                  <span class={styles['error-badge']}>rejected</span>
                </Show>
                <Show when={!text().includes('rejected')}>
                  <span class={styles['ok-badge']}>accepted</span>
                </Show>
              </button>
              <Show when={props.expanded.has(outputKey)}>
                <pre class={ep['plan-output-content']}>{text()}</pre>
              </Show>
            </div>
          )
        }}
      </Show>
    </MessageBlock>
  )
}
