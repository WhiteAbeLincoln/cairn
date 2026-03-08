import { ReactiveMap } from '@solid-primitives/map'
import type {
  DisplayItem,
  ToolResultMap,
  ToolUseMap,
} from '../lib/display-item'
import { mkToggle } from '../lib/types'
import { createContext } from 'solid-js'
import { ReactiveSet } from '@solid-primitives/set'

type ToolUse = Extract<DisplayItem, { kind: 'tool-use' }>
type ToolResult = Extract<DisplayItem, { kind: 'tool-result' }>
export type SessionContextValue = {
  isExpanded: (key: string) => boolean
  toggleExpanded: (key: string) => void

  globalRaw: () => boolean
  displayAsRaw: (key: string) => boolean
  toggleRawDisplay: (key: string) => void

  getToolUse: (key: string) => ToolUse | undefined

  getToolResult: (key: string) => ToolResult | undefined
}

const initialCounterContext: SessionContextValue = {
  isExpanded: () => false,
  toggleExpanded: () => {},

  globalRaw: () => false,
  displayAsRaw: () => false,
  toggleRawDisplay: () => {},

  getToolUse: () => undefined,

  getToolResult: () => undefined,
}

export const SessionContext = createContext(initialCounterContext)
