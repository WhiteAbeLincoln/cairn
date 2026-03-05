// --- Raw JSONL event types (from session log files, not GraphQL) ---

// Fields shared across user, assistant, progress, and system events
interface CommonFields {
  uuid: string
  parentUuid: string | null
  sessionId: string
  timestamp: string
  version?: string
  cwd?: string
  gitBranch?: string
  isSidechain?: boolean
  userType?: string
  slug?: string
}

export interface UserEvent extends CommonFields {
  type: 'user'
  message: {
    role: string
    content: string | ToolResultContent[]
  }
  toolUseResult?: unknown
  sourceToolAssistantUuid?: string
  permissionMode?: string
}

export interface ToolResultContent {
  type: 'tool_result'
  tool_use_id: string
  content: unknown
  is_error?: boolean
}

export interface AssistantEvent extends CommonFields {
  type: 'assistant'
  message: {
    role: string
    type?: string
    model?: string
    id?: string
    content: ContentBlock[]
    stop_reason?: string | null
    stop_sequence?: string | null
    usage?: Usage
  }
  requestId?: string
}

export type ContentBlock = TextBlock | ThinkingBlock | ToolUseBlock | ToolResultBlock

export interface TextBlock {
  type: 'text'
  text: string
}

export interface ThinkingBlock {
  type: 'thinking'
  thinking: string
  signature?: string
}

export interface ToolUseBlock {
  type: 'tool_use'
  id: string
  name: string
  input: unknown
}

export interface ToolResultBlock {
  type: 'tool_result'
  tool_use_id: string
  content: unknown
}

export interface Usage {
  input_tokens?: number
  output_tokens?: number
  cache_creation_input_tokens?: number
  cache_read_input_tokens?: number
  [key: string]: unknown
}

export interface SystemEvent extends CommonFields {
  type: 'system'
  subtype?: string
  durationMs?: number
  isMeta?: boolean
}

export interface ProgressEvent extends CommonFields {
  type: 'progress'
  data: unknown
  toolUseID?: string
  parentToolUseID?: string
}

export interface FileHistorySnapshotEvent {
  type: 'file-history-snapshot'
  messageId: string
  snapshot: unknown
  isSnapshotUpdate?: boolean
}

export interface QueueOperationEvent {
  type: 'queue-operation'
  operation: string
  sessionId: string
  timestamp?: string
  content?: string
}

export type SessionEvent =
  | UserEvent
  | AssistantEvent
  | ProgressEvent
  | SystemEvent
  | FileHistorySnapshotEvent
  | QueueOperationEvent

// Events that have uuid/timestamp and can be rendered in the UI
export type DisplayableEvent = UserEvent | AssistantEvent | SystemEvent | ProgressEvent

// Type guards
export function isUserEvent(e: SessionEvent): e is UserEvent {
  return e.type === 'user'
}

export function isAssistantEvent(e: SessionEvent): e is AssistantEvent {
  return e.type === 'assistant'
}

export function isSystemEvent(e: SessionEvent): e is SystemEvent {
  return e.type === 'system'
}

// Tool result map entry (used across components)
export interface ToolResultEntry {
  content: unknown
  is_error?: boolean
}
