import { createResource, createSignal, createMemo, For, Show, Switch, Match } from 'solid-js'
import { A } from '@solidjs/router'
import { query } from '../lib/graphql'
import type { Session } from '../lib/types'
import styles from './SessionList.module.css'

interface ProjectGroup {
  project: string
  displayName: string
  sessions: Session[]
}

const SESSIONS_QUERY = `{
  sessions {
    sessions {
      meta { id project slug createdAt updatedAt messageCount firstMessage projectPath }
    }
    total
  }
}`

export default function SessionList() {
  const [sessions] = createResource(async () => {
    const data = await query<{ sessions: { sessions: { meta: Session }[]; total: number } }>(SESSIONS_QUERY)
    return data.sessions.sessions.map(s => s.meta)
  })

  const groups = createMemo<ProjectGroup[]>(() => {
    const list = sessions() ?? []
    const map = new Map<string, Session[]>()
    for (const s of list) {
      let arr = map.get(s.project)
      if (!arr) {
        arr = []
        map.set(s.project, arr)
      }
      arr.push(s)
    }
    const result: ProjectGroup[] = []
    for (const [project, projectSessions] of map) {
      const displayName = projectSessions[0]?.projectPath ?? project
      result.push({ project, displayName, sessions: projectSessions })
    }
    result.sort((a, b) => {
      const aMax = a.sessions[0]?.updatedAt ?? ''
      const bMax = b.sessions[0]?.updatedAt ?? ''
      return bMax.localeCompare(aMax)
    })
    return result
  })

  const [collapsed, setCollapsed] = createSignal(new Set<string>())

  function toggleGroup(project: string) {
    setCollapsed((prev) => {
      const next = new Set(prev)
      if (next.has(project)) next.delete(project)
      else next.add(project)
      return next
    })
  }

  function formatDate(iso: string | null): string {
    if (!iso) return ''
    return new Date(iso).toLocaleString()
  }

  return (
    <div class={styles['session-list']}>
      <h1>Sessions</h1>
      <Switch>
        <Match when={sessions.loading}>
          <p class={styles.status}>Loading sessions...</p>
        </Match>
        <Match when={sessions.error}>
          <p class={`${styles.status} ${styles.error}`}>
            Error: {(sessions.error as Error).message}
          </p>
        </Match>
        <Match when={sessions()?.length === 0}>
          <p class={styles.status}>No sessions found.</p>
        </Match>
        <Match when={true}>
          <For each={groups()}>
            {(group) => (
              <div class={styles['project-group']}>
                <button class={styles['group-header']} onClick={() => toggleGroup(group.project)}>
                  <span class={styles.caret}>
                    {collapsed().has(group.project) ? '\u25B8' : '\u25BE'}
                  </span>
                  <span class={styles['group-name']}>{group.displayName}</span>
                  <span class={styles['group-count']}>{group.sessions.length}</span>
                </button>
                <Show when={!collapsed().has(group.project)}>
                  <div class={styles['session-rows']}>
                    <For each={group.sessions}>
                      {(session) => (
                        <A
                          class={styles['session-row']}
                          href={`/session/${session.id}`}
                        >
                          <div class={styles.summary}>
                            {session.firstMessage ?? session.slug ?? '\u2014'}
                          </div>
                          <div class={styles['row-meta']}>
                            <span class={styles.date}>{formatDate(session.updatedAt)}</span>
                            <span class={styles.count}>{session.messageCount} msgs</span>
                          </div>
                        </A>
                      )}
                    </For>
                  </div>
                </Show>
              </div>
            )}
          </For>
        </Match>
      </Switch>
    </div>
  )
}
