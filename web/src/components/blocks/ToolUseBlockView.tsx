// Tool use block dispatcher and individual tool view components.
// Each view renders only content — the parent MessageBlock (via DisplayItemView)
// handles the collapsible wrapper, label, and metadata.

import {
  createMemo,
  createResource,
  For,
  type JSX,
  Show,
  useContext,
} from 'solid-js'
import { A } from '@solidjs/router'
import {
  formatInput,
  stripAnsi,
  truncate,
  parseToolResultParts,
  stripReadLineNumbers,
} from '../../lib/format'
import { fileExtToLang, highlight, highlightBash, highlightToLines } from '../../lib/highlight'
import { parseGrepOutput, type GrepGroup } from '../../lib/grep-parse'
import { JsonTree } from '../../lib/json-tree'
import { contentToString } from '../../lib/session'
import styles from '../SessionView.module.css'
import tu from './ToolUseBlockView.module.css'
import eb from './EditBlockView.module.css'
import ab from './AgentBlockView.module.css'
import cb from './CollapsibleBlock.module.css'
import type { DisplayItem } from '../../lib/display-item'
import { SessionContext } from '../session-context'
import { ToolBlockContext } from './tool-block-context'
import { Dynamic } from 'solid-js/web'

type ToolUse = Extract<DisplayItem, { kind: 'tool-use' }>
type ToolResult = Extract<DisplayItem, { kind: 'tool-result' }>
type ToolEvent = ToolUse | ToolResult

/** Compute the extra label JSX for a tool-use display item from its input data.
 *  Called by DisplayItemView to build the label without requiring child rendering. */
export function toolExtraLabel(evt: DisplayItem): JSX.Element {
  if (evt.kind !== 'tool-use') return null
  const input = evt.content.input as Record<string, unknown> | undefined
  if (!input) return null

  switch (evt.content.name) {
    case 'Read':
    case 'Write':
      return input.file_path ? (
        <span class={styles['tool-filepath']}>{input.file_path as string}</span>
      ) : null
    case 'Edit':
      return (
        <>
          {input.file_path ? (
            <span class={styles['tool-filepath']}>
              {input.file_path as string}
            </span>
          ) : null}
          {input.replace_all ? (
            <span class={eb['info-badge']}>all</span>
          ) : null}
        </>
      )
    case 'Bash':
      return input.description ? (
        <span class={tu['bash-desc']}>{input.description as string}</span>
      ) : null
    case 'Grep':
    case 'Glob':
      return input.pattern ? (
        <span class={styles['tool-filepath']}>{input.pattern as string}</span>
      ) : null
    case 'Agent':
      return (
        <>
          {input.subagent_type ? (
            <>
              <span class={cb['step-dot']}>&middot;</span>
              <span class={styles.step}>{input.subagent_type as string}</span>
            </>
          ) : null}
          {input.description ? (
            <>
              <span class={cb['step-dot']}>&middot;</span>
              <span class={styles.step}>{input.description as string}</span>
            </>
          ) : null}
        </>
      )
    default:
      return null
  }
}

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
  Read: ReadView,
  Write: WriteView,
  Edit: EditView,
  Glob: GlobView,
  Grep: GrepView,
  Agent: AgentView,
}

type ToolViewProps = {
  toolUse?: ToolUse
  toolResult?: ToolResult
  isError: boolean
}

// --- helpers ---

function useExtraLabel(label: () => JSX.Element) {
  const blockCtx = useContext(ToolBlockContext)
  createMemo(() => blockCtx?.setExtraLabel(label()))
}

function toolInput<T>(props: ToolViewProps): T | undefined {
  return props.toolUse?.content.input as T | undefined
}

function toolResultContent(props: ToolViewProps): unknown {
  if (!props.toolResult) return undefined
  // Prefer content.content (structured data with image parts etc.)
  // over event.toolUseResult (often a plain string summary)
  return props.toolResult.content?.content ?? props.toolResult.event.toolUseResult
}

function toolResultString(props: ToolViewProps): string {
  return contentToString(toolResultContent(props))
}

function toolResultIsError(props: ToolViewProps): boolean {
  return !!(props.toolResult?.content as { is_error?: boolean } | undefined)
    ?.is_error
}

// --- GenericToolUse ---

function GenericToolUse(props: ToolViewProps) {
  const input = () => toolInput(props)
  const result = () => toolResultContent(props)

  return (
    <div class={tu['tool-details']}>
      <div class={tu['tool-section']}>
        <div class={tu['tool-section-label']}>Input</div>
        <pre>{formatInput(input())}</pre>
      </div>
      <Show when={props.toolResult}>
        {(_r) => (
          <div class={tu['tool-section']}>
            <div class={tu['tool-section-label']}>Output</div>
            <pre classList={{ [styles['is-error']]: props.isError }}>
              {truncate(contentToString(result()), 5000)}
            </pre>
          </div>
        )}
      </Show>
    </div>
  )
}

// --- AskUserQuestion ---

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
  const input = () => toolInput<BashInput>(props)

  useExtraLabel(() => {
    const desc = input()?.description
    return desc ? <span class={tu['bash-desc']}>{desc}</span> : null
  })

  const output = (): { stdout: string; stderr: string; uuid: string } | undefined => {
    if (!props.toolResult) return undefined
    const raw = props.toolResult.event.toolUseResult
    const uuid = props.toolResult.event.uuid
    if (typeof raw === 'string') {
      return { stdout: raw, stderr: '', uuid }
    }
    const obj = raw as BashOutput | undefined
    if (!obj) return undefined
    return { stdout: obj.stdout ?? '', stderr: obj.stderr ?? '', uuid }
  }

  const hasOutput = () => {
    const o = output()
    return o && (o.stderr || o.stdout) ? o : undefined
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
                  {stripAnsi(contentToString(r().stdout))}
                </pre>
                <Show when={r().stderr}>
                  <p>Stderr</p>
                  <pre
                    class={tu['bash-output']}
                    classList={{ [styles['is-error']]: props.isError }}
                  >
                    {stripAnsi(contentToString(r().stderr))}
                  </pre>
                </Show>
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

// --- Read ---

const MAX_HIGHLIGHT_LENGTH = 50_000

type ReadInput = {
  file_path?: string
  offset?: number
  limit?: number
  pages?: string
}

function ReadView(props: ToolViewProps): JSX.Element {
  const input = () => toolInput<ReadInput>(props)
  const filePath = () => input()?.file_path ?? ''
  const lang = () => fileExtToLang(filePath())

  useExtraLabel(() => {
    const fp = filePath()
    return fp ? <span class={styles['tool-filepath']}>{fp}</span> : null
  })

  const rangeInfo = () => {
    const inp = input()
    if (!inp) return null
    const parts: string[] = []
    if (inp.offset != null) parts.push(`offset ${inp.offset}`)
    if (inp.limit != null) parts.push(`limit ${inp.limit}`)
    if (inp.pages != null) parts.push(`pages ${inp.pages}`)
    return parts.length > 0 ? parts.join(', ') : null
  }

  const resultStr = () => toolResultString(props)
  const isError = () => toolResultIsError(props)
  const parsed = () =>
    props.toolResult ? parseToolResultParts(resultStr()) : null

  const strippedText = () => {
    if (!props.toolResult || isError()) return null
    const p = parsed()
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
      const st = strippedText()
      const l = lang()
      if (!st || !l || st.code.length > MAX_HIGHLIGHT_LENGTH) return null
      return st.code
    },
    (code) => highlight(code!, lang()!),
  )

  return (
    <div class={tu['tool-details']}>
      <Show when={rangeInfo()}>
        {(info) => (
          <div class={tu['tool-section']}>
            <pre>{info()}</pre>
          </div>
        )}
      </Show>
      <Show when={props.toolResult}>
        {(_r) => {
          const st = strippedText()
          const parts = parsed()
          return (
            <div class={tu['tool-section']}>
              {/* Highlighted text output */}
              <Show when={st && !isError()}>
                {(_) => (
                  <Show
                    when={html()}
                    fallback={
                      <pre
                        class={`${styles['highlighted-code']} line-numbers`}
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
              <Show when={isError()}>
                <pre class={styles['is-error']}>
                  {truncate(resultStr(), 5000)}
                </pre>
              </Show>
              {/* Multi-part output with images */}
              <Show when={!st && parts && !isError()}>
                {(_) => (
                  <For each={parts!}>
                    {(part) => (
                      <Show
                        when={
                          part.type === 'image' &&
                          (part as { type: 'image'; dataUri: string })
                        }
                        fallback={
                          <pre>
                            {truncate(
                              (part as { type: 'text'; text: string }).text,
                              5000,
                            )}
                          </pre>
                        }
                      >
                        {(img) => (
                          <img
                            class={styles['tool-image']}
                            src={img().dataUri}
                          />
                        )}
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
  )
}

// --- Write ---

type WriteInput = {
  file_path?: string
  content?: string
}

function WriteView(props: ToolViewProps): JSX.Element {
  const input = () => toolInput<WriteInput>(props)
  const filePath = () => input()?.file_path ?? ''
  const content = () => input()?.content ?? ''
  const lang = () => fileExtToLang(filePath())

  useExtraLabel(() => {
    const fp = filePath()
    return fp ? <span class={styles['tool-filepath']}>{fp}</span> : null
  })

  const [html] = createResource(
    () => {
      const l = lang()
      const c = content()
      return l && c.length <= MAX_HIGHLIGHT_LENGTH ? c : null
    },
    async (code) => {
      const l = lang()
      if (!code || !l) return null
      return highlight(code, l)
    },
  )

  return (
    <div class={tu['tool-details']}>
      <Show
        when={html()}
        fallback={
          <pre class={styles['highlighted-code']}>
            {truncate(content(), 5000)}
          </pre>
        }
      >
        {(h) => <div class={styles['highlighted-code']} innerHTML={h()} />}
      </Show>
      <Show when={toolResultIsError(props)}>
        {(_) => (
          <div class={tu['tool-section']}>
            <div class={tu['tool-section-label']}>Output</div>
            <pre class={styles['is-error']}>
              {truncate(toolResultString(props), 5000)}
            </pre>
          </div>
        )}
      </Show>
    </div>
  )
}

// --- Edit ---

type EditInput = {
  file_path?: string
  old_string?: string
  new_string?: string
  replace_all?: boolean
}

function buildDiffLines(
  oldStr: string,
  newStr: string,
): { type: 'remove' | 'add' | 'context'; text: string }[] {
  const oldLines = oldStr.split('\n')
  const newLines = newStr.split('\n')
  const lines: { type: 'remove' | 'add' | 'context'; text: string }[] = []
  for (const line of oldLines) {
    lines.push({ type: 'remove', text: line })
  }
  for (const line of newLines) {
    lines.push({ type: 'add', text: line })
  }
  return lines
}

function EditView(props: ToolViewProps): JSX.Element {
  const input = () => toolInput<EditInput>(props)
  const filePath = () => input()?.file_path ?? ''
  const hasStrings = () =>
    typeof input()?.old_string === 'string' &&
    typeof input()?.new_string === 'string'

  useExtraLabel(() => {
    const fp = filePath()
    const ra = input()?.replace_all
    return (
      <>
        <Show when={fp}>
          <span class={styles['tool-filepath']}>{fp}</span>
        </Show>
        <Show when={ra}>
          <span class={eb['info-badge']}>all</span>
        </Show>
      </>
    )
  })

  return (
    <div class={tu['tool-details']}>
      <Show
        when={hasStrings()}
        fallback={
          <div class={tu['tool-section']}>
            <div class={tu['tool-section-label']}>Input</div>
            <pre>{formatInput(toolInput(props))}</pre>
          </div>
        }
      >
        <div class={eb['diff-block']}>
          {buildDiffLines(input()!.old_string!, input()!.new_string!).map(
            (line) => (
              <div
                class={eb['diff-line']}
                classList={{
                  [eb['diff-add']]: line.type === 'add',
                  [eb['diff-remove']]: line.type === 'remove',
                }}
              >
                <span class={eb['diff-marker']}>
                  {line.type === 'add'
                    ? '+'
                    : line.type === 'remove'
                      ? '-'
                      : ' '}
                </span>
                <span>{line.text}</span>
              </div>
            ),
          )}
        </div>
      </Show>
      <Show when={toolResultIsError(props)}>
        {(_) => (
          <div class={tu['tool-section']}>
            <div class={tu['tool-section-label']}>Output</div>
            <pre class={styles['is-error']}>
              {truncate(toolResultString(props), 5000)}
            </pre>
          </div>
        )}
      </Show>
    </div>
  )
}

// --- Glob ---

type GlobInput = {
  pattern?: string
  path?: string
}

function GlobView(props: ToolViewProps): JSX.Element {
  const input = () => toolInput<GlobInput>(props)

  useExtraLabel(() => {
    const pattern = input()?.pattern
    return pattern ? (
      <span class={styles['tool-filepath']}>{pattern}</span>
    ) : null
  })

  return (
    <div class={tu['tool-details']}>
      <Show when={props.toolResult}>
        {(_) => (
          <div class={tu['tool-section']}>
            <pre classList={{ [styles['is-error']]: props.isError }}>
              {truncate(toolResultString(props), 5000)}
            </pre>
          </div>
        )}
      </Show>
    </div>
  )
}

// --- Grep ---

type GrepInput = {
  path?: string
  pattern?: string
  output_mode?: string
}

function GrepView(props: ToolViewProps): JSX.Element {
  const input = () => toolInput<GrepInput>(props)

  useExtraLabel(() => {
    const pattern = input()?.pattern
    return pattern ? (
      <span class={styles['tool-filepath']}>{pattern}</span>
    ) : null
  })

  const resultStr = () => toolResultString(props)
  const groups = createMemo(() =>
    props.toolResult ? parseGrepOutput(resultStr()) : [],
  )

  // If the output is files_with_matches or count mode, or an error,
  // just show plain text
  const isPlainMode = () => {
    const mode = input()?.output_mode
    return mode === 'files_with_matches' || mode === 'count'
  }

  const ctx = useContext(SessionContext)
  const inputId = () => `grep-input-${props.toolUse?.id ?? ''}`

  return (
    <div class={tu['tool-details']}>
      <div class={tu['grep-input-section']}>
        <button
          class={styles.toggle}
          onClick={() => ctx.toggleExpanded(inputId())}
        >
          {ctx.isExpanded(inputId()) ? '\u25BE' : '\u25B8'} Input
        </button>
        <Show when={ctx.isExpanded(inputId())}>
          <JsonTree value={input()} defaultExpandDepth={1} />
        </Show>
      </div>
      <Show when={props.toolResult}>
        {(_) => (
          <Show
            when={!toolResultIsError(props) && !isPlainMode()}
            fallback={
              <div class={tu['tool-section']}>
                <pre classList={{ [styles['is-error']]: props.isError }}>
                  {truncate(resultStr(), 5000)}
                </pre>
              </div>
            }
          >
            <div class={tu['grep-results']}>
              <For each={groups()}>
                {(group) => <GrepGroupView group={group} />}
              </For>
            </div>
          </Show>
        )}
      </Show>
    </div>
  )
}

const MAX_GREP_HIGHLIGHT = 50_000

function GrepGroupView(props: { group: GrepGroup }): JSX.Element {
  const lang = () => {
    const fp = props.group.filePath
    return fp ? fileExtToLang(fp) : null
  }

  const code = () => props.group.lines.map((l) => l.content).join('\n')
  const [highlighted] = createResource(
    () => {
      const l = lang()
      const c = code()
      if (!l || c.length > MAX_GREP_HIGHLIGHT) return null
      return { code: c, lang: l }
    },
    (params) => highlightToLines(params!.code, params!.lang),
  )

  return (
    <div class={tu['grep-group']}>
      <Show when={props.group.filePath}>
        {(fp) => <div class={tu['grep-file-header']}>{fp()}</div>}
      </Show>
      <div class={tu['grep-code-block']}>
        <For each={props.group.lines}>
          {(line, i) => {
            const hl = () => highlighted()?.[i()]
            return (
              <div
                class={tu['grep-line']}
                classList={{ [tu['grep-match']]: line.isMatch }}
              >
                <Show when={line.lineNum > 0}>
                  <span class={tu['grep-linenum']}>{line.lineNum}</span>
                </Show>
                <span
                  class={tu['grep-content']}
                  innerHTML={hl() ?? undefined}
                >
                  {hl() ? undefined : line.content}
                </span>
              </div>
            )
          }}
        </For>
      </div>
    </div>
  )
}

// --- Agent ---

function AgentView(props: ToolViewProps): JSX.Element {
  const input = () =>
    toolInput<{
      description?: string
      prompt?: string
      subagent_type?: string
    }>(props)

  useExtraLabel(() => {
    const desc = input()?.description
    const subType = input()?.subagent_type
    return (
      <>
        <Show when={subType}>
          <span class={cb['step-dot']}>&middot;</span>
          <span class={styles.step}>{subType}</span>
        </Show>
        <Show when={desc}>
          <span class={cb['step-dot']}>&middot;</span>
          <span class={styles.step}>{desc}</span>
        </Show>
      </>
    )
  })

  const ctx = useContext(SessionContext)

  const agentId = () => {
    // The agent result typically contains the agent ID
    const result = props.toolResult?.event.toolUseResult as
      | { agentId?: string }
      | undefined
    return result?.agentId
  }

  const outputId = () =>
    props.toolResult ? `${props.toolResult.event.uuid}-agent-output` : ''

  return (
    <div class={ab['agent-expanded']}>
      <Show when={agentId()}>
        {(aid) => (
          <A class={ab['agent-link']} href={`/session/agent-${aid()}`}>
            View subagent session &rarr;
          </A>
        )}
      </Show>
      <Show when={props.toolResult}>
        {(_r) => (
          <div class={ab['agent-output-section']}>
            <button
              class={styles.toggle}
              onClick={() => ctx.toggleExpanded(outputId())}
            >
              {ctx.isExpanded(outputId()) ? '\u25BE' : '\u25B8'} Output
            </button>
            <Show when={ctx.isExpanded(outputId())}>
              <pre class={ab['agent-output']}>
                {truncate(toolResultString(props), 5000)}
              </pre>
            </Show>
          </div>
        )}
      </Show>
    </div>
  )
}
