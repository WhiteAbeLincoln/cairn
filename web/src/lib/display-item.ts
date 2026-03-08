import type { ReactiveMap } from '@solid-primitives/map'
import { enumerate, type ValuesOf } from './util'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type RawEvent = Record<string, any>

export type DisplayItemCommon = {
  event: RawEvent
  id: string
}
export type DisplayItem = (
  | { kind: 'user-message'; content: string }
  | {
      kind: 'assistant-message'
      content: { type: 'text'; text: string }
    }
  | {
      kind: 'thinking'
      content: { type: 'thinking'; thinking: string }
    }
  | {
      kind: 'tool-use'
      content: { type: 'tool_use'; name: string; id: string; input?: unknown }
    }
  | {
      kind: 'tool-result'
      content: { type: 'tool_result'; tool_use_id: string; content: unknown }
    }
  | { kind: 'turn-duration' }
  | { kind: 'compaction'; content: string }
  | { kind: 'other' }
) &
  DisplayItemCommon

export type DisplayMode = 'full' | 'collapsed' | 'grouped' | 'hidden'

// using the mapped object + ValuesOf here
// makes this a discriminated union so we can use Exclude later
export type DisplayItemWithMode =
  | ValuesOf<{
      [k in Exclude<DisplayMode, 'grouped'>]: { item: DisplayItem; mode: k }
    }>
  | { items: DisplayItem[]; mode: 'grouped' }

export type DisplayModeOpts = {
  [k in Exclude<DisplayItem['kind'], 'tool-use'>]: DisplayMode
} & {
  'tool-use':
    | DisplayMode
    | { default: DisplayMode; by_name?: { [name: string]: DisplayMode } }
}

export const defaultDisplayModeOpts: DisplayModeOpts = {
  'user-message': 'full',
  'assistant-message': 'full',
  'turn-duration': 'full',
  thinking: 'grouped',
  'tool-use': {
    default: 'grouped',
    by_name: {
      Bash: 'full',
      AskUserQuestion: 'full',
    },
  },
  // we display the 'tool-use' instead
  // combined with the result
  'tool-result': 'hidden',
  compaction: 'grouped',
  other: 'hidden',
}

export type ToolUseMap = ReactiveMap<
  string,
  Extract<DisplayItem, { kind: 'tool-use' }>
>
export type ToolResultMap = ReactiveMap<
  string,
  Extract<DisplayItem, { kind: 'tool-result' }>
>

function* eventToDisplayItems(
  evt: RawEvent,
  index: number,
): Generator<DisplayItem, void, unknown> {
  switch (evt.type) {
    case 'user':
      yield* userEvent(evt, index)
      return
    case 'assistant':
      yield* assistantEvent(evt, index)
      return
    case 'system': {
      if (evt.subtype === 'turn_duration') {
        yield {
          kind: 'turn-duration',
          event: evt,
          id: `turn-duration-${index}`,
        }
        return
      }
      yield otherEvent(evt, index)
      return
    }
    default:
      yield otherEvent(evt, index)
  }
}

export function* eventsToDisplayItems(
  evts: Iterable<RawEvent>,
  toolUseMap: ToolUseMap,
  toolResultMap: ToolResultMap,
  startIndex = 0,
  displayOpts: DisplayModeOpts = defaultDisplayModeOpts,
): Generator<DisplayItemWithMode, void, unknown> {
  const groupedAcc: DisplayItem[] = []
  for (const [idx, evt] of enumerate(evts, startIndex)) {
    for (const item of eventToDisplayItems(evt, idx)) {
      const displayMode = getDisplayMode(item, displayOpts)

      if (displayMode === 'grouped') {
        groupedAcc.push(item)
        continue
      }
      if (displayMode !== 'hidden' && groupedAcc.length > 0) {
        yield { items: [...groupedAcc], mode: 'grouped' }
        groupedAcc.length = 0
      }

      if (item.kind === 'tool-use') {
        toolUseMap.set(item.content.id, item)
      } else if (item.kind === 'tool-result') {
        toolResultMap.set(item.content.tool_use_id, item)
      }

      yield { item, mode: displayMode }
    }
  }
  if (groupedAcc.length > 0) {
    yield { items: [...groupedAcc], mode: 'grouped' }
  }
}

function getDisplayMode(item: DisplayItem, opts: DisplayModeOpts): DisplayMode {
  if (item.kind === 'tool-use') {
    const toolOpts = opts['tool-use']
    if (typeof toolOpts === 'object') {
      return toolOpts.by_name?.[item.content.name] ?? toolOpts.default
    }
    return toolOpts
  }
  return opts[item.kind]
}

function* userEvent(
  evt: RawEvent,
  index: number,
): Generator<DisplayItem, void, unknown> {
  if (
    typeof evt.message.content === 'string' &&
    evt.isCompactSummary === true
  ) {
    yield {
      kind: 'compaction',
      event: evt,
      content: evt.message.content,
      id: `compaction-${index}`,
    }
    return
  }

  if (
    typeof evt.message.content === 'string' &&
    evt.message.role === 'user' &&
    evt.userType === 'external' &&
    evt.toolUseResult == null &&
    evt.sourceToolAssistantUUID == null &&
    evt.isCompactSummary !== true
  ) {
    yield {
      kind: 'user-message',
      event: evt,
      content: evt.message.content,
      id: `user-${index}`,
    }
    return
  }

  if (Array.isArray(evt.message.content)) {
    const others: unknown[] = []
    for (const [subidx, item] of enumerate(evt.message.content)) {
      if (typeof item === 'object' && (item as any)?.type === 'tool_result') {
        yield {
          kind: 'tool-result',
          event: evt,
          content: item as any,
          id: `tool-result-${index}-${subidx}`,
        }
      } else {
        others.push(item)
      }
    }

    if (others.length > 0) {
      yield otherEvent(
        { ...evt, message: { ...evt.message, content: others } },
        index,
      )
    }
    return
  }

  yield otherEvent(evt, index)
}

function* assistantEvent(
  evt: RawEvent,
  index: number,
): Generator<DisplayItem, void, unknown> {
  if (
    Array.isArray(evt.message.content) &&
    evt.message.model !== '<synthetic>'
  ) {
    const others: unknown[] = []
    for (const [subidx, item] of enumerate(evt.message.content)) {
      const type = typeof item === 'object' ? (item as any)?.type : null
      switch (type) {
        case 'text':
          yield {
            kind: 'assistant-message',
            event: evt,
            content: item as any,
            id: `agent-${index}-${subidx}`,
          }
          break
        case 'thinking':
          yield {
            kind: 'thinking',
            event: evt,
            content: item as any,
            id: `thinking-${index}-${subidx}`,
          }
          break
        case 'tool_use':
          yield {
            kind: 'tool-use',
            event: evt,
            content: item as any,
            id: `tool-use-${index}-${subidx}`,
          }
          break
        default:
          others.push(item)
      }
    }

    if (others.length > 0) {
      yield otherEvent(
        { ...evt, message: { ...evt.message, content: others } },
        index,
      )
    }

    return
  }

  yield otherEvent(evt, index)
}

function otherEvent(evt: RawEvent, index: number): DisplayItem {
  return {
    kind: 'other',
    event: evt,
    id: `other-${index}`,
  }
}
