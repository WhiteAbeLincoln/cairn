// Collapsible tool use block with input/output sections.
// Used inside ContentBlockView (from AssistantMessageView and InternalGroupView).

import {
  createMemo,
  createResource,
  For,
  type JSX,
  Show,
  useContext,
} from 'solid-js'
import { formatInput, stripAnsi, truncate } from '../../lib/format'
import { contentToString } from '../../lib/session'
import styles from '../SessionView.module.css'
import tu from './ToolUseBlockView.module.css'
import type { DisplayItem } from '../../lib/display-item'
import { SessionContext } from '../session-context'
import { Dynamic } from 'solid-js/web'
import { highlightBash } from '../../lib/highlight'

type ToolUse = Extract<DisplayItem, { kind: 'tool-use' }>
type ToolResult = Extract<DisplayItem, { kind: 'tool-result' }>
type ToolEvent = ToolUse | ToolResult

export default function ToolUseBlockView(props: { event: ToolEvent }) {
  const ctx = useContext(SessionContext)

  const tools = createMemo(
    (): { toolResult?: ToolResult; toolUse?: ToolUse } => {
      switch (props.event.kind) {
        case 'tool-use':
          return {
            toolUse: props.event,
            toolResult: ctx.getToolResult(props.event.content.id),
          }
        case 'tool-result':
          return {
            toolResult: props.event,
            toolUse: ctx.getToolUse(props.event.content.tool_use_id),
          }
      }
    },
  )

  const isError = () => {
    const r = tools().toolResult?.content
    return !!(r as any)?.is_error
  }

  const component = () => {
    const toolName = tools().toolUse?.content.name
    if (!toolName) {
      return GenericToolUse
    }
    return toolUseMap[toolName] || GenericToolUse
  }

  return <Dynamic component={component()} {...tools} isError={isError()} />
}

const toolUseMap: {
  [x: string]: undefined | ((p: ToolViewProps) => JSX.Element)
} = {
  AskUserQuestion: AskUserQuestionView,
  Bash: BashView,
}

type ToolViewProps = {
  toolUse?: ToolUse
  toolResult?: ToolResult
  isError: boolean
}

function GenericToolUse(props: ToolViewProps) {
  const toolInput = () =>
    (props.toolUse?.content as { input?: unknown } | undefined)?.input

  const resultContent = () =>
    props.toolResult && props.toolResult.event.toolUseResult
      ? props.toolResult.event.toolUseResult
      : props.toolResult?.content?.content

  return (
    <div class={tu['tool-details']}>
      <div class={tu['tool-section']}>
        <div class={tu['tool-section-label']}>Input</div>
        <pre>{formatInput(toolInput())}</pre>
      </div>
      <Show when={props.toolResult}>
        {(r) => (
          <div class={tu['tool-section']}>
            <div class={tu['tool-section-label']}>Output</div>
            <pre classList={{ [styles['is-error']]: props.isError }}>
              {truncate(contentToString(resultContent()), 5000)}
            </pre>
          </div>
        )}
      </Show>
    </div>
  )
}

// ---- AskUserQuestion ----

type Question = {
  header: string
  multiSelect?: boolean
  options: {
    description: string
    label: string
  }[]
  question: string
}

type QuestionToolResult = {
  questions: Question[]
  answers: Record<string, string | undefined>
}

function AskUserQuestionView(props: ToolViewProps): JSX.Element {
  const questions = () => {
    if (!props.toolUse) {
      // we can get from the toolResult instead
      const result = (
        props.toolResult?.event as { toolUseResult?: QuestionToolResult }
      )?.toolUseResult
      return result?.questions ?? []
    }

    const content = props.toolUse?.content as
      | {
          type: 'tool_use'
          name: string
          id: string
          input: { questions: Question[] }
        }
      | undefined
    return content?.input.questions || []
  }

  const answers = () => {
    if (!props.toolResult) {
      return {}
    }

    const result = (
      props.toolResult.event as { toolUseResult?: QuestionToolResult }
    )?.toolUseResult
    return result?.answers ?? {}
  }

  return (
    <div class={tu['ask-questions']}>
      <For each={questions()}>
        {(q) => {
          const answer = () => answers()[q.question]
          return (
            <div
              class={tu['question-group']}
              data-question={q.header}
              itemscope
              itemtype="https://schema.org/Question"
            >
              <div class={tu['question-header']}>
                <span class={tu['question-badge']} itemprop="name">
                  {q.header}
                </span>
                {q.multiSelect && (
                  <span class={tu['question-multi-badge']}>multi</span>
                )}
              </div>
              <div class={tu['question-text']} itemprop="text">
                {q.question}
              </div>
              <div class={tu['question-options']}>
                <For each={q.options}>
                  {(opt) => {
                    const selected = () => answer() === opt.label
                    return (
                      <div
                        class={tu['question-option']}
                        classList={{ [tu['option-selected']]: selected() }}
                        data-selected={selected() ? 'true' : undefined}
                        itemscope
                        itemtype="https://schema.org/Answer"
                        itemprop={
                          selected() ? 'acceptedAnswer' : 'suggestedAnswer'
                        }
                      >
                        <span class={tu['question-option-indicator']}>
                          {selected() ? '\u25CF' : '\u25CB'}
                        </span>
                        <div>
                          <span
                            class={tu['question-option-label']}
                            itemprop="text"
                          >
                            {opt.label}
                          </span>
                          <span
                            class={tu['question-option-desc']}
                            itemprop="description"
                          >
                            {opt.description}
                          </span>
                        </div>
                      </div>
                    )
                  }}
                </For>
              </div>
            </div>
          )
        }}
      </For>
    </div>
  )
}

// --- Bash ---

type BashInput = {
  command: string
  description: string
}

type BashOutput = {
  interrupted?: boolean
  isImage?: boolean
  noOutputExpected?: boolean
  stderr: string
  stdout: string
}

function BashView(props: ToolViewProps): JSX.Element {
  const input = () => {
    return props.toolUse?.content.input as BashInput | undefined
  }
  const output = () => {
    if (!props.toolResult) {
      return undefined
    }

    const output = props?.toolResult?.event.toolUseResult as
      | BashOutput
      | undefined
    const uuid = props?.toolResult.event.uuid

    return {
      output,
      uuid,
    }
  }

  const hasOutput = () => {
    const o = output()
    return !!(o?.output?.stderr || o?.output?.stdout) ? o : undefined
  }

  const ctx = useContext(SessionContext)

  return (
    <>
      <Show when={input()?.command}>
        {(cmd) => <HighlightedBash code={cmd()} />}
      </Show>
      <Show when={hasOutput()}>
        {(r) => {
          const outputId = () => `${r().uuid}-output`
          return (
            <div class={tu['bash-output-section']}>
              <button
                class={styles.toggle}
                onClick={() => ctx.toggleExpanded(outputId())}
              >
                {ctx.isExpanded(outputId()) ? '\u25BE' : '\u25B8'} Output
              </button>
              <Show when={ctx.isExpanded(outputId())}>
                <p>Stdout</p>
                <pre
                  class={tu['bash-output']}
                  classList={{ [styles['is-error']]: props.isError }}
                >
                  {stripAnsi(contentToString(r().output?.stdout))}
                </pre>
                <p>Stderr</p>
                <pre
                  class={tu['bash-output']}
                  classList={{ [styles['is-error']]: props.isError }}
                >
                  {stripAnsi(contentToString(r().output?.stderr))}
                </pre>
              </Show>
            </div>
          )
        }}
      </Show>
    </>
  )
}

export function HighlightedBash(props: { code: string }) {
  const [html] = createResource(
    () => props.code,
    (code) => highlightBash(code),
  )
  return (
    <Show
      when={html()}
      fallback={
        <pre class={tu['bash-command']}>
          <code>{props.code}</code>
        </pre>
      }
    >
      {(h) => <div class={tu['bash-command']} innerHTML={h()} />}
    </Show>
  )
}
