import type { TypedDocumentString } from './generated/graphql'

const API_BASE = import.meta.env.VITE_API_URL ?? ''

export async function query<TResult, TVars>(
  doc: TypedDocumentString<TResult, TVars>,
  ...args: TVars extends Record<string, never> ? [] : [variables: TVars]
): Promise<TResult> {
  const variables = args[0]
  const res = await fetch(`${API_BASE}/graphql`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ query: doc.toString(), variables }),
  });

  if (!res.ok) {
    throw new Error(`GraphQL request failed: ${res.status}`);
  }

  const json = await res.json();
  if (json.errors) {
    throw new Error(json.errors.map((e: { message: string }) => e.message).join(', '));
  }

  return json.data as TResult;
}

/** Open a GraphQL subscription over WebSocket (graphql-transport-ws protocol). */
export function subscribe<TResult, TVars>(
  doc: TypedDocumentString<TResult, TVars>,
  variables: TVars,
  onData: (data: TResult) => void,
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
        payload: { query: doc.toString(), variables },
      }))
    } else if (msg.type === 'next') {
      onData(msg.payload.data as TResult)
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
