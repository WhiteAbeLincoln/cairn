// Write tool block with syntax-highlighted content based on file_path extension.
// Dispatched from ContentBlockView when block.name === 'Write'.

import { createResource, Show } from 'solid-js'
import { fileExtToLang, highlight } from '../../lib/highlight'
import { truncate } from '../../lib/format'
import { contentToString } from '../../lib/session'
import type { ToolResultEntry } from '../../lib/types'
import CollapsibleBlock from './CollapsibleBlock'
import styles from '../SessionView.module.css'

const MAX_HIGHLIGHT_LENGTH = 50_000

export default function WriteBlockView(props: {
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
    content?: string
  }
  const filePath = input.file_path ?? ''
  const content = input.content ?? ''
  const lang = fileExtToLang(filePath)

  const [html] = createResource(
    () => (props.expanded.has(props.blockKey) && lang && content.length <= MAX_HIGHLIGHT_LENGTH) ? content : null,
    async (code) => {
      if (!code || !lang) return null
      return highlight(code, lang)
    },
  )

  return (
    <CollapsibleBlock
      role="tool"
      meta={{ sessionId: props.sessionId, uuid: props.uuid, tokens: props.tokens, result: props.result }}
      class={styles['tool-block']}
      expanded={props.expanded.has(props.blockKey)}
      toggle={() => props.toggle(props.blockKey)}
      label={
        <>
          <span class={styles.step}>Write</span>
          <Show when={filePath}>
            <span class={styles['tool-filepath']}>{filePath}</span>
          </Show>
        </>
      }
    >
      <div class={styles['tool-details']}>
        <Show
          when={html()}
          fallback={
            <pre class={styles['highlighted-code']}>
              {truncate(content, 5000)}
            </pre>
          }
        >
          {(h) => (
            <div class={styles['highlighted-code']} innerHTML={h()} />
          )}
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
