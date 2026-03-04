// Edit tool block rendered as a unified diff when old_string/new_string are present.
// Dispatched from ContentBlockView when block.name === 'Edit'.

import { Show } from 'solid-js'
import { formatInput, truncate } from '../../lib/format'
import { contentToString } from '../../lib/session'
import type { ToolResultEntry } from '../../lib/types'
import CollapsibleBlock from './CollapsibleBlock'
import eb from './EditBlockView.module.css'
import styles from '../SessionView.module.css'

function buildDiffLines(oldStr: string, newStr: string): { type: 'remove' | 'add' | 'context'; text: string }[] {
  const oldLines = oldStr.split('\n')
  const newLines = newStr.split('\n')
  const lines: { type: 'remove' | 'add' | 'context'; text: string }[] = []

  // Simple diff: show all old lines as removals, all new lines as additions.
  // For a true unified diff we'd need a proper diff algorithm, but for
  // old_string/new_string replacements this is the expected presentation.
  for (const line of oldLines) {
    lines.push({ type: 'remove', text: line })
  }
  for (const line of newLines) {
    lines.push({ type: 'add', text: line })
  }
  return lines
}

export default function EditBlockView(props: {
  blockKey: string
  input: unknown
  result: ToolResultEntry | undefined
  sessionId: string
  uuid: string
  expanded: Set<string>
  toggle: (key: string) => void
  tokens?: number
}) {
  const input = props.input as {
    file_path?: string
    old_string?: string
    new_string?: string
    replace_all?: boolean
  }
  const filePath = input.file_path ?? ''
  const hasStrings = typeof input.old_string === 'string' && typeof input.new_string === 'string'

  return (
    <CollapsibleBlock
      role="tool"
      meta={{ sessionId: props.sessionId, uuid: props.uuid, tokens: props.tokens, result: props.result }}
      class={styles['tool-block']}
      expanded={props.expanded.has(props.blockKey)}
      toggle={() => props.toggle(props.blockKey)}
      label={
        <>
          <span class={styles.step}>Edit</span>
          <Show when={filePath}>
            <span class={styles['tool-filepath']}>{filePath}</span>
          </Show>
          <Show when={input.replace_all}>
            <span class={eb['info-badge']}>all</span>
          </Show>
        </>
      }
    >
      <div class={styles['tool-details']}>
        <Show
          when={hasStrings}
          fallback={
            <div class={styles['tool-section']}>
              <div class={styles['tool-section-label']}>Input</div>
              <pre>{formatInput(props.input)}</pre>
            </div>
          }
        >
          <div class={eb['diff-block']}>
            {buildDiffLines(input.old_string!, input.new_string!).map((line) => (
              <div
                class={eb['diff-line']}
                classList={{
                  [eb['diff-add']]: line.type === 'add',
                  [eb['diff-remove']]: line.type === 'remove',
                }}
              >
                <span class={eb['diff-marker']}>
                  {line.type === 'add' ? '+' : line.type === 'remove' ? '-' : ' '}
                </span>
                <span>{line.text}</span>
              </div>
            ))}
          </div>
        </Show>
        <Show when={props.result?.is_error}>
          {(_) => (
            <div class={styles['tool-section']}>
              <div class={styles['tool-section-label']}>Output</div>
              <pre class={styles['is-error']}>
                {truncate(contentToString(props.result!.content), 5000)}
              </pre>
            </div>
          )}
        </Show>
      </div>
    </CollapsibleBlock>
  )
}
