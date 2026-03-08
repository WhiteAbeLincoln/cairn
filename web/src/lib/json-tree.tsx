import { createSignal, For, Show } from 'solid-js'

interface JsonTreeProps {
  value: unknown
  /** Current nesting depth (0 = top level) */
  depth?: number
  /** Max depth to auto-expand (default 1) */
  defaultExpandDepth?: number
}

function CopyButton(props: { value: unknown }) {
  const [copied, setCopied] = createSignal(false)

  const handleCopy = (e: MouseEvent) => {
    e.stopPropagation()
    const text = typeof props.value === 'string'
      ? props.value
      : JSON.stringify(props.value, null, 2)
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }

  return (
    <button class="json-copy" onClick={handleCopy} title="Copy value">
      {copied() ? '✓' : '⧉'}
    </button>
  )
}

function CopyableValue(props: { value: unknown; children: any }) {
  return (
    <span class="json-copyable">
      {props.children}
      <CopyButton value={props.value} />
    </span>
  )
}

export function JsonTree(props: JsonTreeProps) {
  const depth = () => props.depth ?? 0
  const maxDepth = () => props.defaultExpandDepth ?? 1

  if (props.value === null) return <CopyableValue value={null}><span class="json-null">null</span></CopyableValue>
  if (props.value === undefined) return <CopyableValue value={undefined}><span class="json-null">undefined</span></CopyableValue>

  switch (typeof props.value) {
    case 'string':
      return <CopyableValue value={props.value}><JsonString value={props.value} /></CopyableValue>
    case 'number':
      return <CopyableValue value={props.value}><span class="json-number">{String(props.value)}</span></CopyableValue>
    case 'boolean':
      return <CopyableValue value={props.value}><span class="json-bool">{String(props.value)}</span></CopyableValue>
    default:
      break
  }

  if (Array.isArray(props.value)) {
    return (
      <CopyableValue value={props.value}>
        <JsonArray
          items={props.value}
          depth={depth()}
          defaultExpandDepth={maxDepth()}
        />
      </CopyableValue>
    )
  }

  if (typeof props.value === 'object') {
    return (
      <CopyableValue value={props.value}>
        <JsonObject
          obj={props.value as Record<string, unknown>}
          depth={depth()}
          defaultExpandDepth={maxDepth()}
        />
      </CopyableValue>
    )
  }

  return <CopyableValue value={props.value}><span>{String(props.value)}</span></CopyableValue>
}

function JsonString(props: { value: string }) {
  const MAX_LEN = 200
  const [expanded, setExpanded] = createSignal(false)
  const truncated = () => props.value.length > MAX_LEN && !expanded()

  return (
    <span class="json-string">
      "
      {truncated() ? props.value.slice(0, MAX_LEN) : props.value}
      {truncated() && (
        <button class="json-expand-str" onClick={() => setExpanded(true)}>
          ...{props.value.length - MAX_LEN} more
        </button>
      )}
      "
    </span>
  )
}

function JsonObject(props: {
  obj: Record<string, unknown>
  depth: number
  defaultExpandDepth: number
}) {
  const keys = () => Object.keys(props.obj)
  const [open, setOpen] = createSignal(props.depth < props.defaultExpandDepth)

  return (
    <span class="json-container">
      <button class="json-fold" onClick={() => setOpen(!open())}>
        {open() ? '\u25BE' : '\u25B8'}
      </button>
      <Show
        when={open()}
        fallback={
          <span class="json-collapsed" onClick={() => setOpen(true)}>
            {'{ '}<span class="json-badge">{keys().length} keys</span>{' }'}
          </span>
        }
      >
        {'{'}
        <div class="json-indent">
          <For each={keys()}>
            {(key, i) => (
              <div class="json-entry">
                <span class="json-key">"{key}"</span>
                {': '}
                <JsonTree
                  value={props.obj[key]}
                  depth={props.depth + 1}
                  defaultExpandDepth={props.defaultExpandDepth}
                />
                {i() < keys().length - 1 ? ',' : ''}
              </div>
            )}
          </For>
        </div>
        {'}'}
      </Show>
    </span>
  )
}

function JsonArray(props: {
  items: unknown[]
  depth: number
  defaultExpandDepth: number
}) {
  const [open, setOpen] = createSignal(props.depth < props.defaultExpandDepth)

  return (
    <span class="json-container">
      <button class="json-fold" onClick={() => setOpen(!open())}>
        {open() ? '\u25BE' : '\u25B8'}
      </button>
      <Show
        when={open()}
        fallback={
          <span class="json-collapsed" onClick={() => setOpen(true)}>
            {'[ '}<span class="json-badge">{props.items.length} items</span>{' ]'}
          </span>
        }
      >
        {'['}
        <div class="json-indent">
          <For each={props.items}>
            {(item, i) => (
              <div class="json-entry">
                <JsonTree
                  value={item}
                  depth={props.depth + 1}
                  defaultExpandDepth={props.defaultExpandDepth}
                />
                {i() < props.items.length - 1 ? ',' : ''}
              </div>
            )}
          </For>
        </div>
        {']'}
      </Show>
    </span>
  )
}
