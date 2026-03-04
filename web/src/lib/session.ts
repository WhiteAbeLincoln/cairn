import type { DisplayableEvent, SessionEvent, ToolUseBlock } from './types'

type AnyEvent = DisplayableEvent | SessionEvent

export function getToolUseBlock(msg: AnyEvent, name: string): ToolUseBlock | null {
  if (msg.type !== 'assistant') return null
  return (
    (msg.message.content.find(
      (b): b is ToolUseBlock => b.type === 'tool_use' && b.name === name,
    ) as ToolUseBlock) ?? null
  )
}

export function getAgentBlock(msg: AnyEvent): ToolUseBlock | null {
  return getToolUseBlock(msg, 'Task') ?? getToolUseBlock(msg, 'Agent')
}

export function hasUserFacingText(msg: AnyEvent): boolean {
  if (msg.type !== 'assistant') return false
  return msg.message.content.some((b) => b.type === 'text')
}

export function totalTokens(msg: AnyEvent): number | undefined {
  if (msg.type !== 'assistant') return undefined
  const u = msg.message.usage
  if (u) {
    return (u.input_tokens ?? 0) + (u.output_tokens ?? 0)
  }
}

export function compactSteps(steps: string[]): { name: string; count: number }[] {
  const result: { name: string; count: number }[] = []
  for (const s of steps) {
    const last = result[result.length - 1]
    if (last && last.name === s) {
      last.count++
    } else {
      result.push({ name: s, count: 1 })
    }
  }
  return result
}

/** Convert raw tool result content to a display string. */
export function contentToString(v: unknown): string {
  if (typeof v === 'string') return v
  if (v == null) return ''
  return JSON.stringify(v)
}
