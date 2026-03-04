// Read tool call with file path label, syntax highlighting, and image rendering.
// Dispatched from ContentBlockView when block.name === 'Read'.

import { createResource, Show, For } from 'solid-js'
import { truncate, parseToolResultParts, stripReadLineNumbers } from '../../lib/format'
import { fileExtToLang, highlight } from '../../lib/highlight'
import { contentToString } from '../../lib/session'
import type { ToolResultEntry } from '../../lib/types'
import CollapsibleBlock from './CollapsibleBlock'
import styles from '../SessionView.module.css'

const MAX_HIGHLIGHT_LENGTH = 50_000

export default function ReadBlockView(props: {
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
    offset?: number
    limit?: number
    pages?: string
  }
  const filePath = input.file_path ?? ''
  const lang = fileExtToLang(filePath)
  const rangeInfo = () => {
    const parts: string[] = []
    if (input.offset != null) parts.push(`offset ${input.offset}`)
    if (input.limit != null) parts.push(`limit ${input.limit}`)
    if (input.pages != null) parts.push(`pages ${input.pages}`)
    return parts.length > 0 ? parts.join(', ') : null
  }

  const resultStr = () => props.result ? contentToString(props.result.content) : ''
  const parsed = () => props.result ? parseToolResultParts(resultStr()) : null

  // For plain text results (non-image), strip line numbers and highlight
  const strippedText = () => {
    if (!props.result || props.result.is_error) return null
    const p = parsed()
    // Only handle simple text (no images, no multi-part)
    if (p && p.length === 1 && p[0].type === 'text') {
      return stripReadLineNumbers(p[0].text)
    }
    if (!p) {
      return stripReadLineNumbers(resultStr())
    }
    return null
  }

  const [html] = createResource(
    () => {
      const expanded = props.expanded.has(props.blockKey)
      const st = strippedText()
      if (!expanded || !st || !lang || st.code.length > MAX_HIGHLIGHT_LENGTH) return null
      return st.code
    },
    (code) => highlight(code!, lang!),
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
          <span class={styles.step}>Read</span>
          <Show when={filePath}>
            <span class={styles['tool-filepath']}>{filePath}</span>
          </Show>
        </>
      }
    >
      <div class={styles['tool-details']}>
        <Show when={rangeInfo()}>
          {(info) => (
            <div class={styles['tool-section']}>
              <pre>{info()}</pre>
            </div>
          )}
        </Show>
        <Show when={props.result}>
          {(r) => {
            const st = strippedText()
            const parts = parsed()
            return (
              <div class={styles['tool-section']}>
                {/* Highlighted text output */}
                <Show when={st && !r().is_error}>
                  {(_) => (
                    <Show
                      when={html()}
                      fallback={
                        <pre class={`${styles['highlighted-code']} line-numbers`}
                          style={{ '--start-line': st!.startLine }}
                        >
                          {st!.code}
                        </pre>
                      }
                    >
                      {(h) => (
                        <div
                          class={`${styles['highlighted-code']} line-numbers`}
                          style={{ '--start-line': st!.startLine }}
                          innerHTML={h()}
                        />
                      )}
                    </Show>
                  )}
                </Show>
                {/* Error output */}
                <Show when={r().is_error}>
                  <pre class={styles['is-error']}>
                    {truncate(contentToString(r().content), 5000)}
                  </pre>
                </Show>
                {/* Multi-part output with images */}
                <Show when={!st && parts && !r().is_error}>
                  {(_) => (
                    <For each={parts!}>
                      {(part) => (
                        <Show when={part.type === 'image' && part as { type: 'image'; dataUri: string }}
                          fallback={
                            <pre>
                              {truncate((part as { type: 'text'; text: string }).text, 5000)}
                            </pre>
                          }
                        >
                          {(img) => <img class={styles['tool-image']} src={img().dataUri} />}
                        </Show>
                      )}
                    </For>
                  )}
                </Show>
              </div>
            )
          }}
        </Show>
      </div>
    </CollapsibleBlock>
  )
}
