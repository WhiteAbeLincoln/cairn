const API_BASE = import.meta.env.VITE_API_URL ?? ''

export async function query<T>(q: string, variables?: Record<string, unknown>): Promise<T> {
  const res = await fetch(`${API_BASE}/graphql`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ query: q, variables }),
  });

  if (!res.ok) {
    throw new Error(`GraphQL request failed: ${res.status}`);
  }

  const json = await res.json();
  if (json.errors) {
    throw new Error(json.errors.map((e: { message: string }) => e.message).join(', '));
  }

  return json.data as T;
}

/** Open a GraphQL subscription over WebSocket (graphql-transport-ws protocol). */
export function subscribe<T>(
  q: string,
  variables: Record<string, unknown>,
  onData: (data: T) => void,
  onError?: (err: unknown) => void,
): () => void {
  const wsUrl = API_BASE
    ? API_BASE.replace(/^http/, 'ws') + '/graphql/ws'
    : `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/graphql/ws`
  const ws = new WebSocket(wsUrl, 'graphql-transport-ws')
  let disposed = false

  ws.onopen = () => {
    ws.send(JSON.stringify({ type: 'connection_init', payload: {} }))
  }

  ws.onmessage = (event) => {
    const msg = JSON.parse(event.data)
    if (msg.type === 'connection_ack') {
      ws.send(JSON.stringify({
        id: '1',
        type: 'subscribe',
        payload: { query: q, variables },
      }))
    } else if (msg.type === 'next') {
      onData(msg.payload.data as T)
    } else if (msg.type === 'error') {
      onError?.(msg.payload)
    }
  }

  ws.onerror = (event) => {
    onError?.(event)
  }

  return () => {
    if (disposed) return
    disposed = true
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ id: '1', type: 'complete' }))
    }
    ws.close()
  }
}
