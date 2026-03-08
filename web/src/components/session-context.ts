import type { DisplayItem } from '../lib/display-item'
import { createContext } from 'solid-js'

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
