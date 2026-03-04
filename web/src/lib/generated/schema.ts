export type Maybe<T> = T | null;
export type InputMaybe<T> = Maybe<T>;
export type Exact<T extends { [key: string]: unknown }> = { [K in keyof T]: T[K] };
export type MakeOptional<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]?: Maybe<T[SubKey]> };
export type MakeMaybe<T, K extends keyof T> = Omit<T, K> & { [SubKey in K]: Maybe<T[SubKey]> };
export type MakeEmpty<T extends { [key: string]: unknown }, K extends keyof T> = { [_ in K]?: never };
export type Incremental<T> = T | { [P in keyof T]?: P extends ' $fragmentName' | '__typename' ? T[P] : never };
/** All built-in and custom scalars, mapped to their actual values */
export type Scalars = {
  ID: { input: string; output: string; }
  String: { input: string; output: string; }
  Boolean: { input: boolean; output: boolean; }
  Int: { input: number; output: number; }
  Float: { input: number; output: number; }
  /**
   * Implement the DateTime<Utc> scalar
   *
   * The input/output is a string in RFC3339 format.
   */
  DateTime: { input: string; output: string; }
  /** A scalar that can represent any JSON value. */
  JSON: { input: unknown; output: unknown; }
};

export type AgentMapping = {
  __typename?: 'AgentMapping';
  agentId: Scalars['String']['output'];
  toolUseId: Scalars['String']['output'];
};

export type AssistantEvent = CoreEvent & Event & {
  __typename?: 'AssistantEvent';
  agentId: Maybe<Scalars['String']['output']>;
  apiError: Maybe<Scalars['JSON']['output']>;
  children: Array<Event>;
  cwd: Scalars['String']['output'];
  error: Maybe<Scalars['JSON']['output']>;
  gitBranch: Maybe<Scalars['String']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  isSidechain: Scalars['Boolean']['output'];
  message: Scalars['JSON']['output'];
  parent: Maybe<Event>;
  parentUuid: Maybe<Scalars['String']['output']>;
  raw: Scalars['JSON']['output'];
  requestId: Maybe<Scalars['String']['output']>;
  sessionId: Scalars['String']['output'];
  slug: Maybe<Scalars['String']['output']>;
  timestamp: Scalars['String']['output'];
  type: Scalars['String']['output'];
  userType: Scalars['String']['output'];
  uuid: Scalars['String']['output'];
  version: Scalars['String']['output'];
};

export type CoreEvent = {
  children: Array<Event>;
  cwd: Scalars['String']['output'];
  gitBranch: Maybe<Scalars['String']['output']>;
  isSidechain: Scalars['Boolean']['output'];
  parent: Maybe<Event>;
  parentUuid: Maybe<Scalars['String']['output']>;
  sessionId: Scalars['String']['output'];
  slug: Maybe<Scalars['String']['output']>;
  timestamp: Scalars['String']['output'];
  userType: Scalars['String']['output'];
  uuid: Scalars['String']['output'];
  version: Scalars['String']['output'];
};

export type Event = {
  apiError: Maybe<Scalars['JSON']['output']>;
  error: Maybe<Scalars['JSON']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  raw: Scalars['JSON']['output'];
  type: Scalars['String']['output'];
};

export type FileHistoryEvent = Event & {
  __typename?: 'FileHistoryEvent';
  apiError: Maybe<Scalars['JSON']['output']>;
  error: Maybe<Scalars['JSON']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  isSnapshotUpdate: Scalars['Boolean']['output'];
  messageId: Scalars['String']['output'];
  raw: Scalars['JSON']['output'];
  snapshot: Scalars['JSON']['output'];
  type: Scalars['String']['output'];
};

export type PageInput = {
  limit?: Scalars['Int']['input'];
  offset?: Scalars['Int']['input'];
};

export type ProgressEvent = CoreEvent & Event & {
  __typename?: 'ProgressEvent';
  agentId: Maybe<Scalars['String']['output']>;
  apiError: Maybe<Scalars['JSON']['output']>;
  children: Array<Event>;
  cwd: Scalars['String']['output'];
  data: Maybe<Scalars['JSON']['output']>;
  error: Maybe<Scalars['JSON']['output']>;
  gitBranch: Maybe<Scalars['String']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  isSidechain: Scalars['Boolean']['output'];
  parent: Maybe<Event>;
  parentToolUseID: Maybe<Scalars['String']['output']>;
  parentUuid: Maybe<Scalars['String']['output']>;
  raw: Scalars['JSON']['output'];
  sessionId: Scalars['String']['output'];
  slug: Maybe<Scalars['String']['output']>;
  timestamp: Scalars['String']['output'];
  toolUseID: Maybe<Scalars['String']['output']>;
  type: Scalars['String']['output'];
  userType: Scalars['String']['output'];
  uuid: Scalars['String']['output'];
  version: Scalars['String']['output'];
};

export type Query = {
  __typename?: 'Query';
  /** Load a session by ID. */
  session: Maybe<Session>;
  /** List discovered sessions, optionally filtered by project name and paginated. */
  sessions: SessionsResult;
};


export type QuerySessionArgs = {
  id: Scalars['String']['input'];
};


export type QuerySessionsArgs = {
  page?: InputMaybe<PageInput>;
  project?: InputMaybe<Scalars['String']['input']>;
};

export type QueueOperationEvent = Event & {
  __typename?: 'QueueOperationEvent';
  apiError: Maybe<Scalars['JSON']['output']>;
  content: Maybe<Scalars['String']['output']>;
  error: Maybe<Scalars['JSON']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  operation: Scalars['String']['output'];
  raw: Scalars['JSON']['output'];
  sessionId: Scalars['String']['output'];
  timestamp: Scalars['String']['output'];
  type: Scalars['String']['output'];
};

export type Session = {
  __typename?: 'Session';
  /** Mapping from tool_use_id to agent_id for subagent calls. */
  agentMap: Array<AgentMapping>;
  /** Load session events, optionally paginated. */
  events: SessionEventsData;
  meta: SessionMeta;
  /** The raw JSONL content of the session file. */
  rawLog: Scalars['String']['output'];
};


export type SessionEventsArgs = {
  page?: InputMaybe<PageInput>;
};

export type SessionEventsData = {
  __typename?: 'SessionEventsData';
  events: Array<Event>;
  total: Scalars['Int']['output'];
};

export type SessionMeta = {
  __typename?: 'SessionMeta';
  agentId: Maybe<Scalars['String']['output']>;
  createdAt: Maybe<Scalars['DateTime']['output']>;
  /** Absolute path to the session's .jsonl file on disk. */
  filePath: Maybe<Scalars['String']['output']>;
  firstMessage: Maybe<Scalars['String']['output']>;
  id: Scalars['String']['output'];
  isSidechain: Scalars['Boolean']['output'];
  messageCount: Scalars['Int']['output'];
  parentSessionId: Maybe<Scalars['String']['output']>;
  project: Scalars['String']['output'];
  projectPath: Maybe<Scalars['String']['output']>;
  slug: Maybe<Scalars['String']['output']>;
  updatedAt: Maybe<Scalars['DateTime']['output']>;
};

export type SessionsResult = {
  __typename?: 'SessionsResult';
  sessions: Array<Session>;
  total: Scalars['Int']['output'];
};

export type SystemEvent = CoreEvent & Event & {
  __typename?: 'SystemEvent';
  apiError: Maybe<Scalars['JSON']['output']>;
  cause: Maybe<Scalars['JSON']['output']>;
  children: Array<Event>;
  compactMetadata: Maybe<Scalars['JSON']['output']>;
  content: Maybe<Scalars['String']['output']>;
  cwd: Scalars['String']['output'];
  durationMs: Maybe<Scalars['Int']['output']>;
  error: Maybe<Scalars['JSON']['output']>;
  gitBranch: Maybe<Scalars['String']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  isMeta: Maybe<Scalars['Boolean']['output']>;
  isSidechain: Scalars['Boolean']['output'];
  level: Maybe<Scalars['String']['output']>;
  logicalParentUuid: Maybe<Scalars['String']['output']>;
  maxRetries: Maybe<Scalars['Int']['output']>;
  parent: Maybe<Event>;
  parentUuid: Maybe<Scalars['String']['output']>;
  raw: Scalars['JSON']['output'];
  retryAttempt: Maybe<Scalars['Int']['output']>;
  retryInMs: Maybe<Scalars['Int']['output']>;
  sessionId: Scalars['String']['output'];
  slug: Maybe<Scalars['String']['output']>;
  subtype: Maybe<Scalars['String']['output']>;
  timestamp: Scalars['String']['output'];
  type: Scalars['String']['output'];
  url: Maybe<Scalars['String']['output']>;
  userType: Scalars['String']['output'];
  uuid: Scalars['String']['output'];
  version: Scalars['String']['output'];
};

export type UnknownEvent = Event & {
  __typename?: 'UnknownEvent';
  apiError: Maybe<Scalars['JSON']['output']>;
  error: Maybe<Scalars['JSON']['output']>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  raw: Scalars['JSON']['output'];
  type: Scalars['String']['output'];
};

export type UserEvent = CoreEvent & Event & {
  __typename?: 'UserEvent';
  agentId: Maybe<Scalars['String']['output']>;
  apiError: Maybe<Scalars['JSON']['output']>;
  children: Array<Event>;
  cwd: Scalars['String']['output'];
  error: Maybe<Scalars['JSON']['output']>;
  gitBranch: Maybe<Scalars['String']['output']>;
  imagePasteIds: Maybe<Array<Scalars['String']['output']>>;
  isApiErrorMessage: Maybe<Scalars['Boolean']['output']>;
  isCompactSummary: Maybe<Scalars['Boolean']['output']>;
  isMeta: Maybe<Scalars['Boolean']['output']>;
  isSidechain: Scalars['Boolean']['output'];
  isVisibleInTranscriptOnly: Maybe<Scalars['Boolean']['output']>;
  message: Scalars['JSON']['output'];
  parent: Maybe<Event>;
  parentUuid: Maybe<Scalars['String']['output']>;
  permissionMode: Maybe<Scalars['String']['output']>;
  planContent: Maybe<Scalars['String']['output']>;
  raw: Scalars['JSON']['output'];
  sessionId: Scalars['String']['output'];
  slug: Maybe<Scalars['String']['output']>;
  sourceToolAssistantUUID: Maybe<Scalars['String']['output']>;
  thinkingMetadata: Maybe<Scalars['JSON']['output']>;
  timestamp: Scalars['String']['output'];
  todos: Maybe<Array<Scalars['JSON']['output']>>;
  toolUseResult: Maybe<Scalars['JSON']['output']>;
  type: Scalars['String']['output'];
  userType: Scalars['String']['output'];
  uuid: Scalars['String']['output'];
  version: Scalars['String']['output'];
};
