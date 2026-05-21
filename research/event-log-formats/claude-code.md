# Claude Code Session Log Format

## Overview

- **Agent:** Claude Code (Anthropic)
- **Location:** `~/.claude/projects/<project-hash>/<session-uuid>.jsonl`
- **Format:** JSONL (JSON Lines), one event per line, append-only
- **Structure:** Flat event stream with `parentUuid` linked list forming conversation tree

## Storage

Session logs live under `~/.claude/projects/`. Each project directory is named with a hash derived from the project path (e.g., `-Users-abe-Projects-myproject`). Inside, each session is a single `.jsonl` file named `<uuid>.jsonl`.

## Event Types

Each line is a JSON object. The top-level `type` field discriminates between event types:

| Type | Description |
|------|-------------|
| `user` | User-typed message or tool result |
| `assistant` | Model response (thinking, text, tool_use content blocks) |
| `system` | System events (e.g., turn duration) |
| `progress` | Progress updates (hooks, subagents) |
| `file-history-snapshot` | File backup tracking |
| `last-prompt` | Session tail marker |

## Common Fields

Most events share these top-level fields:

```
parentUuid      - UUID of the previous event (null for first message)
isSidechain     - boolean, whether this is a side conversation
userType        - "external"
cwd             - working directory
sessionId       - session UUID
version         - Claude Code version (e.g., "2.1.73")
gitBranch       - current git branch
slug            - session slug (appears after first tool use)
type            - event type discriminator
uuid            - this event's UUID
timestamp       - ISO 8601 timestamp
```

## User Events

User events come in two forms:

### Text input (user-typed message)

`message.content` is a **plain string**.

Additional fields:
- `promptId` - unique ID for this prompt
- `permissionMode` - e.g., "bypassPermissions"

### Tool result

`message.content` is an **array** of `tool_result` objects.

Additional fields:
- `toolUseResult` - structured metadata about the tool result
- `sourceToolAssistantUUID` - links back to the assistant event that triggered this tool

Tool result content blocks have:
- `tool_use_id` - matches the `id` from the corresponding `tool_use` content block
- `type` - always `"tool_result"`
- `content` - the tool output (string)

## Assistant Events

Assistant events contain `message.content` as an **array** of content blocks. A single streaming API response is split into multiple JSONL events, one per content block type, all sharing the same `message.id`.

### Content block types

| Block type | Fields | Notes |
|------------|--------|-------|
| `thinking` | `thinking`, `signature` | `thinking` text is always empty (redacted); only `signature` preserved |
| `text` | `text` | The assistant's text output |
| `tool_use` | `id`, `name`, `input`, `caller` | Tool invocation; `id` is used to match with `tool_result` |

### Message envelope fields

```
message.model         - model identifier (e.g., "claude-opus-4-6")
message.id            - API message ID (shared across content blocks in same response)
message.type          - always "message"
message.role          - always "assistant"
message.content       - array of content blocks
message.stop_reason   - null (streaming), "end_turn", "stop_sequence", "tool_use"
message.stop_sequence - null or the stop sequence that triggered
message.usage         - token usage statistics
requestId             - API request ID
```

### Usage object

```
usage.input_tokens                  - input token count
usage.output_tokens                 - output token count
usage.cache_creation_input_tokens   - tokens used for cache creation
usage.cache_read_input_tokens       - tokens read from cache
usage.cache_creation.ephemeral_5m_input_tokens
usage.cache_creation.ephemeral_1h_input_tokens
usage.service_tier                  - "standard"
usage.server_tool_use.web_search_requests
usage.server_tool_use.web_fetch_requests
```

## System Events

```
type     - "system"
subtype  - event subtype (e.g., "turn_duration")
```

Known subtypes:
- `turn_duration` - marks end of a turn, with `durationMs` field

## Progress Events

```
type              - "progress"
data.type         - progress subtype
toolUseID         - associated tool use ID
parentToolUseID   - parent tool use ID
```

Known `data.type` values:
- `hook_progress` - hook execution progress, with `hookEvent`, `hookName`, `command`
- `agent_progress` - subagent progress, with nested `message` and `agentId`

## File History Snapshots

```
type              - "file-history-snapshot"
messageId         - associated message UUID
snapshot          - object with trackedFileBackups
isSnapshotUpdate  - false for initial, true for updates
```

## Last Prompt

Terminal event in a session:
```
type        - "last-prompt"
lastPrompt  - the text of the last prompt (e.g., "continue")
sessionId   - session UUID
```

## Third-Party Type Definitions

Claude Code is closed source (distributed as a compiled npm package). No official schema is published. The types below are reverse-engineered from real session files by third-party projects.

### Rust: `delexw/claude-code-trace`

Source: [src-tauri/src/parser/entry.rs](https://github.com/delexw/claude-code-trace/blob/main/src-tauri/src/parser/entry.rs)

```rust
#[derive(Debug, Deserialize, Default)]
pub struct Entry {
    #[serde(default, rename = "type")]
    pub entry_type: String,       // "user" | "assistant" | "system" | "progress" | "summary" | "file-history-snapshot" | "queue-operation" | "attachment"
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default, rename = "isSidechain")]
    pub is_sidechain: bool,
    #[serde(default, rename = "isMeta")]
    pub is_meta: bool,
    #[serde(default)]
    pub message: EntryMessage,
    #[serde(default)]
    pub cwd: String,
    #[serde(default, rename = "gitBranch")]
    pub git_branch: String,
    #[serde(default, rename = "permissionMode")]
    pub permission_mode: String,
    #[serde(default, rename = "toolUseResult")]
    pub tool_use_result: Option<Value>,
    #[serde(default, rename = "sourceToolUseID")]
    pub source_tool_use_id: String,
    #[serde(default, rename = "leafUuid")]
    pub leaf_uuid: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default, rename = "requestId")]
    pub request_id: String,
    #[serde(default, rename = "teamName")]
    pub team_name: String,
    #[serde(default, rename = "agentName")]
    pub agent_name: String,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub subtype: String,
    #[serde(default, rename = "hookEvent")]
    pub hook_event: String,
    #[serde(default, rename = "hookName")]
    pub hook_name: String,
    #[serde(default, rename = "hookCount")]
    pub hook_count: u32,
    #[serde(default, rename = "hookInfos")]
    pub hook_infos: Option<Value>,
    #[serde(default, rename = "preventedContinuation")]
    pub prevented_continuation: bool,
    #[serde(default)]
    pub attachment: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct EntryMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: EntryUsage,
}

#[derive(Debug, Deserialize, Default)]
pub struct EntryUsage {
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_input_tokens: i64,
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
}
```

Content blocks from the same repo ([src-tauri/src/parser/patterns.rs](https://github.com/delexw/claude-code-trace/blob/main/src-tauri/src/parser/patterns.rs)):

```rust
#[derive(Debug, Deserialize, Default)]
pub struct ContentBlockJSON {
    #[serde(default, rename = "type")]
    pub block_type: String,       // "text" | "tool_use" | "tool_result" | "thinking"
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub thinking: String,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default, rename = "tool_use_id")]
    pub tool_use_id: String,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default, rename = "is_error")]
    pub is_error: bool,
}
```

### TypeScript: `depetrol/claude-analysis`

Source: [src/types/trace.ts](https://github.com/depetrol/claude-analysis/blob/main/src/types/trace.ts)

```typescript
export interface ContentBlock {
  type: 'text' | 'tool_use' | 'tool_result' | 'thinking';
  text?: string;
  thinking?: string;
  id?: string;
  name?: string;
  input?: Record<string, unknown>;
  tool_use_id?: string;
  content?: string | ContentBlock[];
}

export interface Message {
  role?: string;
  content?: string | ContentBlock[];
  model?: string;
  id?: string;
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    cache_read_input_tokens?: number;
    cache_creation_input_tokens?: number;
  };
}

export interface ProgressInner {
  type: 'user' | 'assistant';
  timestamp?: string;
  message?: Message;
}

export interface TraceEvent {
  uuid?: string;
  parentUuid?: string | null;
  type: 'user' | 'assistant' | 'progress' | 'summary' | 'file-history-snapshot' | 'system' | 'queue-operation';
  isSidechain?: boolean;
  sessionId?: string;
  agentId?: string;
  slug?: string;
  timestamp?: string;
  message?: Message;
  data?: {
    message?: ProgressInner;
    type?: string;
    hookEvent?: string;
    hookName?: string;
    agentId?: string;
    prompt?: string;
  };
  parentToolUseID?: string;
  toolUseID?: string;
  version?: string;
  gitBranch?: string;
  cwd?: string;
}
```

### TypeScript: `dudufcb1/claude-code-diffs`

Source: [src/types/session-events.ts](https://github.com/dudufcb1/claude-code-diffs/blob/main/src/types/session-events.ts)

Adds types not present in the other sources:

```typescript
export interface ThinkingMetadata {
  level: string;
  disabled: boolean;
  triggers: any[];
}

export interface FileHistorySnapshot {
  messageId: string;
  trackedFileBackups: Record<string, string>;
  timestamp: string;
}
```

### Official: `@anthropic-ai/claude-code` npm package

The npm package ships `sdk-tools.d.ts` which defines tool input/output schemas (e.g., `BashInput`, `FileReadInput`, `GrepInput`, etc.) but does **not** define the session JSONL event envelope types.

## Sources

- Local session files at `~/.claude/projects/`
- [delexw/claude-code-trace](https://github.com/delexw/claude-code-trace) -- Rust session log parser (Tauri app)
- [depetrol/claude-analysis](https://github.com/depetrol/claude-analysis) -- TypeScript session log types
- [dudufcb1/claude-code-diffs](https://github.com/dudufcb1/claude-code-diffs) -- TypeScript session event types
- [lebovic/agent-quickstart](https://github.com/lebovic/agent-quickstart) -- TypeScript SDK streaming event types
- [eqtylab/agent-console](https://github.com/eqtylab/agent-console) -- TypeScript processed session event types
- `@anthropic-ai/claude-code` npm package -- official tool I/O schemas only

---

## Examples

### Example: Full session excerpt

Source: local file `~/.claude/projects/-Users-abe-Projects-REDACTED/9413eb94-e7fe-4a90-8769-20bd12213aa9.jsonl`

#### Line 1 -- `file-history-snapshot` (session start)

```json
{"type":"file-history-snapshot","messageId":"c57ca0c3-d209-4946-9c4a-114ea39a14c8","snapshot":{"messageId":"c57ca0c3-d209-4946-9c4a-114ea39a14c8","trackedFileBackups":{},"timestamp":"2026-03-11T23:19:25.049Z"},"isSnapshotUpdate":false}
```

#### Line 2 -- `user` (initial text prompt)

```json
{"parentUuid":null,"isSidechain":false,"promptId":"44490f28-387c-4776-b887-9c82c77152d1","userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","type":"user","message":{"role":"user","content":"Implement the cache-aside pattern for the redis cache implementation. It should check the redis cache for data matching the key and set it if not present. See https://github.com/redis/rueidis/tree/main/rueidisaside for an example, though this won't work directly because it doesn't handle hset. Make sure to handle the case of cache sets from concurrent goroutines (cache stampede). Create unit tests for your work first (red-green TDD)."},"uuid":"c57ca0c3-d209-4946-9c4a-114ea39a14c8","timestamp":"2026-03-11T23:19:25.049Z","permissionMode":"bypassPermissions"}
```

#### Line 3 -- `assistant` with `thinking` block

```json
{"parentUuid":"c57ca0c3-d209-4946-9c4a-114ea39a14c8","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","message":{"model":"claude-opus-4-6","id":"msg_01CcLTnDHFjoDHhCvk4XkxU4","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"","signature":"EqQCCkYICxgCKkBFRcrqnIfvJBeneeKyVkMe8X4LT+j+6i9cQ+iMcH2hevorgFLAx0SCkZyyFRvJL//UwQN0600BstX3rGA0JxuFEgzN3UmsIyzsFW/w/AgaDNWZjHgz6OFtyq6cRCIwRdX7aNLv7nPDzDm1JehFrKkB9o9bmpLU1vz0+h2+A7xHZFanAs4PBGbqrinA0JBsKosBD25uYAociFjI137OnKIKrKOMyyT/TYmlPkE9iwLWOX9lsGbP4GLhUCZHUP2H/uF/SjK+vb5eqeA6CbxVXRJ560QHcyR7TFkGqrEe1HgcvBpdPfpkV2Gv6QdhQZAY3ATV/TkxxqwPaV0vgtOroZ2sh3965FdUyq3R2MNRbSB5emUEv2J28Mx7vk1vIhgB"}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":7885,"cache_read_input_tokens":8809,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":7885},"output_tokens":9,"service_tier":"standard","inference_geo":"not_available"}},"requestId":"req_011CYx9am7zWCnretjSrbJeC","type":"assistant","uuid":"4eef7514-c775-41a6-b994-fbd7882ed3d1","timestamp":"2026-03-11T23:19:28.186Z"}
```

#### Line 4 -- `assistant` with `text` block

```json
{"parentUuid":"4eef7514-c775-41a6-b994-fbd7882ed3d1","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","message":{"model":"claude-opus-4-6","id":"msg_01CcLTnDHFjoDHhCvk4XkxU4","type":"message","role":"assistant","content":[{"type":"text","text":"Let me first read the existing cache files to understand the current implementation."}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":7885,"cache_read_input_tokens":8809,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":7885},"output_tokens":9,"service_tier":"standard","inference_geo":"not_available"}},"requestId":"req_011CYx9am7zWCnretjSrbJeC","type":"assistant","uuid":"c032e6a8-0760-4299-871f-3dce0f084983","timestamp":"2026-03-11T23:19:29.084Z"}
```

#### Line 5 -- `assistant` with `tool_use` block

```json
{"parentUuid":"c032e6a8-0760-4299-871f-3dce0f084983","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","slug":"concurrent-sniffing-peach","message":{"model":"claude-opus-4-6","id":"msg_01CcLTnDHFjoDHhCvk4XkxU4","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01PL2XFV83PiqcNDb6Anv4Dh","name":"Read","input":{"file_path":"/Users/abe/Projects/REDACTED/cache/redis.go"},"caller":{"type":"direct"}}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":7885,"cache_read_input_tokens":8809,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":7885},"output_tokens":9,"service_tier":"standard","inference_geo":"not_available"}},"requestId":"req_011CYx9am7zWCnretjSrbJeC","type":"assistant","uuid":"f9534911-1b2e-49ea-9641-9220b4e8d24f","timestamp":"2026-03-11T23:19:29.578Z"}
```

#### Line 6 -- `progress` (hook)

```json
{"parentUuid":"f9534911-1b2e-49ea-9641-9220b4e8d24f","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","slug":"concurrent-sniffing-peach","type":"progress","data":{"type":"hook_progress","hookEvent":"PostToolUse","hookName":"PostToolUse:Read","command":"callback"},"parentToolUseID":"toolu_01PL2XFV83PiqcNDb6Anv4Dh","toolUseID":"toolu_01PL2XFV83PiqcNDb6Anv4Dh","timestamp":"2026-03-11T23:19:29.587Z","uuid":"58f18dcf-69ea-414d-8cc4-4738fe26bd21"}
```

#### Line 7 -- `user` with `tool_result`

```json
{"parentUuid":"f9534911-1b2e-49ea-9641-9220b4e8d24f","isSidechain":false,"promptId":"44490f28-387c-4776-b887-9c82c77152d1","userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","slug":"concurrent-sniffing-peach","type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_01PL2XFV83PiqcNDb6Anv4Dh","type":"tool_result","content":"     1\tpackage cache\n     2\t\n     3\timport (\n..."}]},"uuid":"828a8763-34cb-4b62-a787-aaa23791a5b4","timestamp":"2026-03-11T23:19:29.587Z","toolUseResult":{"type":"text","file":{"filePath":"/Users/abe/Projects/REDACTED/cache/redis.go","content":"...","numLines":187,"startLine":1,"totalLines":187}},"sourceToolAssistantUUID":"f9534911-1b2e-49ea-9641-9220b4e8d24f"}
```

#### Line 48 -- `progress` (subagent)

```json
{"parentUuid":"1b0e2c73-f18d-4ec6-af9a-80b3750729bf","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","slug":"concurrent-sniffing-peach","type":"progress","data":{"message":{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Fetch the source code from https://github.com/redis/rueidis/blob/main/rueidisaside/aside.go..."}]},"uuid":"cc2decf7-7b03-4184-a7e0-399f304ab5a4","timestamp":"2026-03-11T23:19:45.905Z"},"type":"agent_progress","prompt":"...","agentId":"addfd18abec6c691d"},"toolUseID":"agent_msg_01717ioE4NowkiGzAEVK5H3W","parentToolUseID":"toolu_0191uYUdm2cWHBw3zNfU9egS","uuid":"ee909409-22b6-4d27-b9f4-8b6b765b30da","timestamp":"2026-03-11T23:19:45.907Z"}
```

#### Line 70 -- `file-history-snapshot` (update)

```json
{"type":"file-history-snapshot","messageId":"5ce1fe70-6d86-4caa-bdaa-652480f1a47e","snapshot":{"messageId":"c57ca0c3-d209-4946-9c4a-114ea39a14c8","trackedFileBackups":{"cache/redis_test.go":{"backupFileName":"7b39fc5e734cef1f@v1","version":1,"backupTime":"2026-03-11T23:20:53.982Z"}},"timestamp":"2026-03-11T23:19:25.049Z"},"isSnapshotUpdate":true}
```

#### Line 135 -- `assistant` with `stop_reason: "end_turn"`

```json
{"parentUuid":"d371e7aa-0e33-4f5d-b0c1-1af1ccd980fb","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","slug":"concurrent-sniffing-peach","message":{"model":"claude-opus-4-6","id":"msg_01WSPWMp2hDyXZeGjU3oSco3","type":"message","role":"assistant","content":[{"type":"text","text":"Here's a summary of what was done:\n\n**Implementation** (`cache/redis.go`):\n- Added a `*singleflight.Group` field to `RedisCache`, shared across namespaced instances\n- `CacheAside` checks the cache first via `Get`, returning immediately on hit\n- On miss, uses `singleflight.Do` keyed by the namespaced key to deduplicate concurrent fetches..."}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"cache_creation_input_tokens":632,"cache_read_input_tokens":53413,"output_tokens":296,"server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},"service_tier":"standard","cache_creation":{"ephemeral_1h_input_tokens":632,"ephemeral_5m_input_tokens":0},"inference_geo":"","iterations":[],"speed":"standard"}},"requestId":"req_011CYx9p8TFR9vZMX1JrSshK","type":"assistant","uuid":"ac0bdc65-bc78-4820-a0d7-19bfd9a148fb","timestamp":"2026-03-11T23:22:34.723Z"}
```

#### Line 136 -- `system` (turn duration)

```json
{"parentUuid":"ac0bdc65-bc78-4820-a0d7-19bfd9a148fb","isSidechain":false,"userType":"external","cwd":"/Users/abe/Projects/REDACTED","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9","version":"2.1.73","gitBranch":"grpc-runtime-caching","slug":"concurrent-sniffing-peach","type":"system","subtype":"turn_duration","durationMs":189701,"timestamp":"2026-03-11T23:22:34.755Z","uuid":"9ce4a5da-827b-4e95-8c35-814f5e6f6d60","isMeta":false}
```

#### Line 248 -- `last-prompt`

```json
{"type":"last-prompt","lastPrompt":"continue","sessionId":"9413eb94-e7fe-4a90-8769-20bd12213aa9"}
```

# Event Keys Grouped by `type`

## user (31257)
| count | percent | key | type |
|-------|-------|-----|-----|
      31257 | 100.0% |  userType                    | string
      31257 | 100.0% |  message                     | object
      31257 | 100.0% |  type                        | string
      31257 | 100.0% |  gitBranch                   | string
      31257 | 100.0% |  uuid                        | string
      31257 | 100.0% |  sessionId                   | string
      31257 | 100.0% |  isSidechain                 | boolean
      31257 | 100.0% |  version                     | string
      31257 | 100.0% |  cwd                         | string
      31257 | 100.0% |  timestamp                   | string
      30886 |  98.8% |  slug                        | string
      30700 |  98.2% |  parentUuid                  | string
      28592 |  91.5% |  sourceToolAssistantUUID     | string
      21055 |  67.4% |  promptId                    | string
      18661 |  59.7% |  toolUseResult               | object,string,array
      16840 |  53.9% |  agentId                     | string
      14457 |  46.3% |  entrypoint                  | string
       1719 |   5.5% |  permissionMode              | string
        108 |   0.3% |  isMeta                      | boolean
         74 |   0.2% |  isVisibleInTranscriptOnly   | boolean
         74 |   0.2% |  isCompactSummary            | boolean
         68 |   0.2% |  sourceToolUseID             | string
         42 |   0.1% |  origin                      | object
         17 |   0.1% |  imagePasteIds               | array
          2 |   0.0% |  planContent                 | string
          1 |   0.0% |  todos                       | array

## queue-operation (747)
| count | percent | key | type |
|-------|-------|-----|-----|
        747 | 100.0% |  timestamp                    |string
        747 | 100.0% |  sessionId                    |string
        747 | 100.0% |  type                         |string
        747 | 100.0% |  operation                    |string
        369 |  49.4% |  content                      |string

## system (955)
| count | percent | key | type |
|-------|-------|-----|-----|
        955 | 100.0% |  userType                     |string
        955 | 100.0% |  gitBranch                    |string
        955 | 100.0% |  isSidechain                  |boolean
        955 | 100.0% |  cwd                          |string
        955 | 100.0% |  timestamp                    |string
        955 | 100.0% |  subtype                      |string
        955 | 100.0% |  sessionId                    |string
        955 | 100.0% |  version                      |string
        955 | 100.0% |  uuid                         |string
        955 | 100.0% |  type                         |string
        943 |  98.7% |  isMeta                       |boolean
        929 |  97.3% |  slug                         |string
        881 |  92.3% |  parentUuid                   |string
        796 |  83.4% |  durationMs                   |number
        426 |  44.6% |  entrypoint                   |string
        241 |  25.2% |  messageCount                 |number
        235 |  24.6% |  agentId                      |string
        147 |  15.4% |  content                      |string
        138 |  14.5% |  level                        |string
         73 |   7.6% |  compactMetadata              |object
         73 |   7.6% |  logicalParentUuid            |string
         21 |   2.2% |  url                          |string
         12 |   1.3% |  error                        |object
         12 |   1.3% |  retryAttempt                 |number
         12 |   1.3% |  maxRetries                   |number
         12 |   1.3% |  retryInMs                    |number
         10 |   1.0% |  cause                        |object
          5 |   0.5% |  upgradeNudge                 |string

## progress (37888)
| count | percent | key | type |
|-------|-------|-----|-----|
      37888 | 100.0% |  gitBranch                   | string
      37888 |100.0% |  isSidechain                  |boolean
      37888 |100.0% |  uuid                         |string
      37888 |100.0% |  type                         |string
      37888 |100.0% |  timestamp                    |string
      37888 |100.0% |  sessionId                    |string
      37888 |100.0% |  cwd                          |string
      37888 |100.0% |  userType                     |string
      37888 |100.0% |  parentToolUseID              |string
      37888 |100.0% |  data                         |object
      37888 |100.0% |  toolUseID                    |string
      37888 |100.0% |  version                      |string
      37844 | 99.9% |  parentUuid                   |string
      37780 | 99.7% |  slug                         |string
      16743 | 44.2% |  entrypoint                   |string
      14306 | 37.8% |  agentId                      |string

## assistant (44404)
| count | percent | key | type |
|-------|-------|-----|-----|
      44404 |100.0% |userType                    | string
      44404 |100.0% |gitBranch                    |string
      44404 |100.0% |timestamp                    |string
      44404 |100.0% |uuid                         |string
      44404 |100.0% |type                         |string
      44404 |100.0% |cwd                         | string
      44404 |100.0% |sessionId                    |string
      44404 |100.0% |message                     | object
      44404 |100.0% |version                     | string
      44404 |100.0% |isSidechain                 | boolean
      44404 |100.0% |parentUuid                  | string
      44370 | 99.9% |requestId                   | string
      43888 | 98.8% |slug                        | string
      23909 | 53.8% |agentId                     | string
      20727 | 46.7% |entrypoint                  | string
         34 |  0.1% |isApiErrorMessage           | boolean
          7 |  0.0% |error                       | string
          3 |  0.0% |apiError                    | string

## last-prompt (124)
| count | percent | key | type |
|-------|-------|-----|-----|
        124 |100.0% |  sessionId                    |string
        124 |100.0% |  type                         |string
        124 |100.0% |  lastPrompt                   |string

## file-history-snapshot (2200)
| count | percent | key | type |
|-------|-------|-----|-----|
       2200 |100.0% |  messageId                    |string
       2200 |100.0% |  snapshot                     |object
       2200 |100.0% |  type                         |string
       2200 |100.0% |  isSnapshotUpdate             |boolean
