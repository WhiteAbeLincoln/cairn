import { createSignal, createResource, createMemo, createEffect, For, Show, Switch, Match } from 'solid-js'
import { useParams, A } from '@solidjs/router'
import { query } from '../lib/graphql'
import { truncate } from '../lib/format'
import type { SessionEvent, DisplayableEvent, ToolResultContent, ToolResultEntry } from '../lib/types'
import { getToolUseBlock, getAgentBlock, hasUserFacingText, contentToString } from '../lib/session'
import UserMessageView from './blocks/UserMessageView'
import AssistantMessageView from './blocks/AssistantMessageView'
import { AskQuestionBlockView } from './blocks/AskUserQuestionView'
import ExitPlanModeView from './blocks/ExitPlanModeView'
import BashBlockView from './blocks/BashBlockView'
import AgentBlockView from './blocks/AgentBlockView'
import TaskListView from './blocks/TaskListView'
import InternalGroupView from './blocks/InternalGroupView'
import SystemMessageView from './blocks/SystemMessageView'
import Prose from './Prose'
import styles from './SessionView.module.css'

const SESSION_INFO_QUERY = `query ($id: String!) {
  sessionInfo(id: $id) {
    id isSidechain parentSessionId agentId firstMessage
  }
}`

const AGENT_MAP_QUERY = `query ($id: String!) {
  sessionAgentMap(id: $id) { toolUseId agentId }
}`

const SESSION_QUERY = `query ($id: String!) {
  session(id: $id) { events total }
}`

type DisplayItem =
  | { kind: 'user'; msg: DisplayableEvent }
  | { kind: 'assistant'; msg: DisplayableEvent }
  | { kind: 'ask-user-question'; msg: DisplayableEvent }
  | { kind: 'exit-plan-mode'; msg: DisplayableEvent }
  | { kind: 'bash'; msg: DisplayableEvent }
  | { kind: 'agent'; msg: DisplayableEvent }
  | { kind: 'internal-group'; key: string; steps: string[]; tokens: number; msgs: DisplayableEvent[] }
  | { kind: 'task-list'; tasks: Map<string, { subject: string; status: string }>; msgs: DisplayableEvent[] }
  | { kind: 'system'; msg: DisplayableEvent }

// --- Main component ---

export default function SessionView() {
  const params = useParams<{ id: string }>()

  interface SessionInfoData {
    id: string
    isSidechain: boolean
    parentSessionId: string | null
    agentId: string | null
    firstMessage: string | null
  }

  const [sessionInfo] = createResource(
    () => params.id,
    async (id) => {
      const data = await query<{ sessionInfo: SessionInfoData | null }>(SESSION_INFO_QUERY, { id })
      return data.sessionInfo
    },
  )

  const [messages] = createResource(
    () => params.id,
    async (id) => {
      const data = await query<{ session: { events: SessionEvent[]; total: number } | null }>(SESSION_QUERY, { id })
      return data.session?.events ?? []
    },
  )

  const [agentMapRaw] = createResource(
    () => params.id,
    async (id) => {
      const data = await query<{
        sessionAgentMap: { toolUseId: string; agentId: string }[]
      }>(AGENT_MAP_QUERY, { id })
      return data.sessionAgentMap ?? []
    },
  )

  const agentMap = createMemo(() => {
    const map = new Map<string, string>()
    for (const m of agentMapRaw() ?? []) {
      map.set(m.toolUseId, m.agentId)
    }
    return map
  })

  const toolResults = createMemo(() => {
    const map = new Map<string, ToolResultEntry>()
    for (const msg of messages() ?? []) {
      if (msg.type === 'user' && Array.isArray(msg.message.content)) {
        for (const r of msg.message.content as ToolResultContent[]) {
          map.set(r.tool_use_id, { content: r.content, is_error: r.is_error })
        }
      }
    }
    return map
  })

  const displayItems = createMemo<DisplayItem[]>(() => {
    const items: DisplayItem[] = []
    let internalAcc: DisplayableEvent[] = []
    const taskMap = new Map<string, { subject: string; status: string }>()
    let taskAcc: DisplayableEvent[] = []

    function flushTasks() {
      if (taskAcc.length === 0) return
      const snapshot = new Map<string, { subject: string; status: string }>()
      for (const [k, v] of taskMap) snapshot.set(k, { ...v })
      items.push({ kind: 'task-list', tasks: snapshot, msgs: taskAcc })
      taskAcc = []
    }

    function flushInternal() {
      if (internalAcc.length === 0) return
      const steps: string[] = []
      let tokens = 0
      const first = internalAcc[0]
      const key = `ig-${first.type === 'assistant' || first.type === 'user' || first.type === 'system' || first.type === 'progress' ? first.uuid : ''}`
      for (const m of internalAcc) {
        if (m.type === 'assistant') {
          for (const b of m.message.content) {
            if (b.type === 'thinking') steps.push('Thinking')
            else if (b.type === 'tool_use') steps.push(b.name)
          }
          const u = m.message.usage
          if (u) tokens += (u.input_tokens ?? 0) + (u.output_tokens ?? 0)
        }
      }
      items.push({ kind: 'internal-group', key, steps, tokens, msgs: internalAcc })
      internalAcc = []
    }

    for (const m of messages() ?? []) {
      if (m.type === 'user' && typeof m.message.content === 'string') {
        flushInternal()
        flushTasks()
        items.push({ kind: 'user', msg: m })
      } else if (m.type === 'assistant') {
        if (hasUserFacingText(m)) {
          flushInternal()
          flushTasks()
          items.push({ kind: 'assistant', msg: m })
        } else if (getToolUseBlock(m, 'AskUserQuestion')) {
          flushInternal()
          flushTasks()
          items.push({ kind: 'ask-user-question', msg: m })
        } else if (getToolUseBlock(m, 'ExitPlanMode')) {
          flushInternal()
          flushTasks()
          items.push({ kind: 'exit-plan-mode', msg: m })
        } else if (getToolUseBlock(m, 'Bash')) {
          flushInternal()
          flushTasks()
          items.push({ kind: 'bash', msg: m })
        } else if (getAgentBlock(m)) {
          flushInternal()
          flushTasks()
          items.push({ kind: 'agent', msg: m })
        } else if (m.message.content.some((b) => b.type === 'tool_use' && (b.name === 'TaskCreate' || b.name === 'TaskUpdate'))) {
          flushInternal()
          for (const b of m.message.content) {
            if (b.type === 'tool_use' && b.name === 'TaskCreate') {
              const input = b.input as { subject?: string }
              const result = toolResults().get(b.id)
              const idMatch = contentToString(result?.content).match(/Task #(\d+)/)
              const taskId = idMatch ? idMatch[1] : b.id
              taskMap.set(taskId, { subject: input.subject ?? '', status: 'pending' })
            } else if (b.type === 'tool_use' && b.name === 'TaskUpdate') {
              const input = b.input as { taskId?: string; status?: string }
              if (input.taskId && input.status) {
                const existing = taskMap.get(input.taskId)
                if (existing) {
                  existing.status = input.status
                } else {
                  taskMap.set(input.taskId, { subject: `Task ${input.taskId}`, status: input.status })
                }
              }
            }
          }
          taskAcc.push(m)
        } else {
          flushTasks()
          internalAcc.push(m)
        }
      } else if (m.type === 'system' && m.subtype === 'turn_duration') {
        flushInternal()
        flushTasks()
        items.push({ kind: 'system', msg: m })
      }
    }
    flushInternal()
    flushTasks()
    return items
  })

  // Map message uuids to their parent group key (for expanding collapsed groups)
  const uuidToGroup = createMemo(() => {
    const map = new Map<string, string>()
    for (const item of displayItems()) {
      if (item.kind === 'internal-group') {
        for (const msg of item.msgs) {
          if ('uuid' in msg) map.set(msg.uuid as string, item.key)
        }
      }
    }
    return map
  })

  // Scroll to hash target once content is rendered
  let scrolledToHash = false
  createEffect(() => {
    const items = displayItems()
    const exp = expanded()
    if (scrolledToHash || items.length === 0 || !location.hash) return

    const id = location.hash.slice(1)

    // If target is inside a collapsed group, expand it first
    const groupKey = uuidToGroup().get(id)
    if (groupKey && !exp.has(groupKey)) {
      setExpanded((prev) => new Set([...prev, groupKey]))
      return
    }

    requestAnimationFrame(() => {
      const el = document.getElementById(id)
      if (el) {
        scrolledToHash = true
        el.scrollIntoView({ behavior: 'smooth' })
      }
    })
  })

  const [expanded, setExpanded] = createSignal(new Set<string>())

  function toggle(key: string) {
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  }

  return (
    <div class={styles['session-view']}>
      <header>
        <Show when={sessionInfo()?.isSidechain && sessionInfo()?.parentSessionId}>
          <A class={styles['back-link']} href={`/session/${sessionInfo()!.parentSessionId}`}>&larr; Parent</A>
        </Show>
        <Show when={!sessionInfo()?.isSidechain}>
          <A class={styles['back-link']} href="/">&larr; Back</A>
        </Show>
        <h1>
          <A class={styles['session-link']} href={`/session/${params.id}/raw`}>
            <Show when={sessionInfo()?.isSidechain} fallback={<>Session {params.id.slice(0, 8)}</>}>
              Subagent {params.id.replace('agent-', '').slice(0, 8)}
            </Show>
          </A>
        </h1>
      </header>

      <Show when={sessionInfo()?.isSidechain}>
        {(_) => {
          const info = sessionInfo()!
          const lastAssistantText = createMemo(() => {
            const msgs = messages() ?? []
            for (let j = msgs.length - 1; j >= 0; j--) {
              const m = msgs[j]
              if (m.type === 'assistant') {
                for (const b of m.message.content) {
                  if (b.type === 'text') return b.text
                }
              }
            }
            return null
          })
          return (
            <div class={styles['subagent-header']} data-role="subagent-header">
              <div class={styles['subagent-section']}>
                <div class={styles['subagent-label']}>Prompt</div>
                <div class={styles['subagent-text']}>{info.firstMessage}</div>
              </div>
              <Show when={lastAssistantText()}>
                {(text) => (
                  <div class={styles['subagent-section']}>
                    <div class={styles['subagent-label']}>Output</div>
                    <Prose
                      text={truncate(text(), 3000)}
                      class={styles['subagent-text']}
                    />
                  </div>
                )}
              </Show>
            </div>
          )
        }}
      </Show>

      <Switch>
        <Match when={messages.loading}>
          <p class={styles.status}>Loading session...</p>
        </Match>
        <Match when={messages.error}>
          <p class={`${styles.status} ${styles.error}`}>
            Error: {(messages.error as Error).message}
          </p>
        </Match>
        <Match when={true}>
          <div class={styles.messages}>
            <For each={displayItems()}>
              {(item) => (
                <Switch>
                  <Match when={item.kind === 'user' && item as DisplayItem & { kind: 'user' }}>
                    {(i) => <UserMessageView msg={i().msg} sessionId={params.id} />}
                  </Match>

                  <Match when={item.kind === 'assistant' && (item as DisplayItem & { kind: 'assistant' })}>
                    {(i) => (
                      <AssistantMessageView
                        msg={i().msg}
                        sessionId={params.id}
                        expanded={expanded()}
                        toggle={toggle}
                        toolResults={toolResults()}
                      />
                    )}
                  </Match>

                  <Match when={item.kind === 'ask-user-question' && (item as DisplayItem & { kind: 'ask-user-question' })}>
                    {(i) => (
                      <AskQuestionBlockView
                        msg={i().msg}
                        sessionId={params.id}
                        toolResults={toolResults()}
                      />
                    )}
                  </Match>

                  <Match when={item.kind === 'exit-plan-mode' && (item as DisplayItem & { kind: 'exit-plan-mode' })}>
                    {(i) => (
                      <ExitPlanModeView
                        msg={i().msg}
                        sessionId={params.id}
                        toolResults={toolResults()}
                        expanded={expanded()}
                        toggle={toggle}
                      />
                    )}
                  </Match>

                  <Match when={item.kind === 'bash' && (item as DisplayItem & { kind: 'bash' })}>
                    {(i) => (
                      <BashBlockView
                        msg={i().msg}
                        sessionId={params.id}
                        toolResults={toolResults()}
                        expanded={expanded()}
                        toggle={toggle}
                      />
                    )}
                  </Match>

                  <Match when={item.kind === 'agent' && (item as DisplayItem & { kind: 'agent' })}>
                    {(i) => (
                      <AgentBlockView
                        msg={i().msg}
                        sessionId={params.id}
                        toolResults={toolResults()}
                        agentMap={agentMap()}
                        expanded={expanded()}
                        toggle={toggle}
                      />
                    )}
                  </Match>

                  <Match when={item.kind === 'task-list' && (item as DisplayItem & { kind: 'task-list' })}>
                    {(i) => {
                      const firstMsg = i().msgs[0]
                      const uuid = 'uuid' in firstMsg ? (firstMsg.uuid as string) : ''
                      const key = `task-list-${uuid}`
                      return (
                        <TaskListView
                          tasks={i().tasks}
                          sessionId={params.id}
                          uuid={uuid}
                          expanded={expanded().has(key)}
                          toggle={() => toggle(key)}
                        />
                      )
                    }}
                  </Match>

                  <Match when={item.kind === 'internal-group' && (item as DisplayItem & { kind: 'internal-group' })}>
                    {(i) => (
                      <InternalGroupView
                        groupKey={i().key}
                        steps={i().steps}
                        tokens={i().tokens}
                        msgs={i().msgs}
                        sessionId={params.id}
                        expanded={expanded()}
                        toggle={toggle}
                        toolResults={toolResults()}
                      />
                    )}
                  </Match>

                  <Match when={item.kind === 'system' && (item as DisplayItem & { kind: 'system' })}>
                    {(i) => <SystemMessageView msg={i().msg} />}
                  </Match>
                </Switch>
              )}
            </For>
          </div>
        </Match>
      </Switch>
    </div>
  )
}
