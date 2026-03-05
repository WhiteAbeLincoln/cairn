/* eslint-disable */
import * as types from './graphql';



/**
 * Map of all GraphQL operations in the project.
 *
 * This map has several performance disadvantages:
 * 1. It is not tree-shakeable, so it will include all operations in the project.
 * 2. It is not minifiable, so the string of a GraphQL query will be multiple times inside the bundle.
 * 3. It does not support dead code elimination, so it will add unused operations.
 *
 * Therefore it is highly recommended to use the babel or swc plugin for production.
 * Learn more about it here: https://the-guild.dev/graphql/codegen/plugins/presets/preset-client#reducing-bundle-size
 */
type Documents = {
    "query Sessions {\n  sessions {\n    sessions {\n      meta {\n        id\n        project\n        slug\n        createdAt\n        updatedAt\n        messageCount\n        firstMessage\n        projectPath\n      }\n    }\n    total\n  }\n}\n\nquery Session($id: String!) {\n  session(id: $id) {\n    meta {\n      id\n      isSidechain\n      parentSessionId\n      agentId\n      firstMessage\n    }\n    events {\n      events {\n        raw\n      }\n      total\n    }\n    agentMap {\n      toolUseId\n      agentId\n    }\n  }\n}\n\nsubscription SessionEvents($id: String!) {\n  sessionEvents(id: $id) {\n    raw\n  }\n}\n\nquery SessionPage($id: String!, $page: PageInput) {\n  session(id: $id) {\n    meta {\n      id\n      filePath\n    }\n    events(page: $page) {\n      events {\n        raw\n      }\n      total\n    }\n  }\n}\n\nquery SessionMeta($id: String!) {\n  session(id: $id) {\n    meta {\n      id\n      filePath\n    }\n  }\n}\n\nquery RawLog($id: String!) {\n  session(id: $id) {\n    rawLog\n  }\n}": typeof types.SessionsDocument,
};
const documents: Documents = {
    "query Sessions {\n  sessions {\n    sessions {\n      meta {\n        id\n        project\n        slug\n        createdAt\n        updatedAt\n        messageCount\n        firstMessage\n        projectPath\n      }\n    }\n    total\n  }\n}\n\nquery Session($id: String!) {\n  session(id: $id) {\n    meta {\n      id\n      isSidechain\n      parentSessionId\n      agentId\n      firstMessage\n    }\n    events {\n      events {\n        raw\n      }\n      total\n    }\n    agentMap {\n      toolUseId\n      agentId\n    }\n  }\n}\n\nsubscription SessionEvents($id: String!) {\n  sessionEvents(id: $id) {\n    raw\n  }\n}\n\nquery SessionPage($id: String!, $page: PageInput) {\n  session(id: $id) {\n    meta {\n      id\n      filePath\n    }\n    events(page: $page) {\n      events {\n        raw\n      }\n      total\n    }\n  }\n}\n\nquery SessionMeta($id: String!) {\n  session(id: $id) {\n    meta {\n      id\n      filePath\n    }\n  }\n}\n\nquery RawLog($id: String!) {\n  session(id: $id) {\n    rawLog\n  }\n}": types.SessionsDocument,
};

/**
 * The graphql function is used to parse GraphQL queries into a document that can be used by GraphQL clients.
 */
export function graphql(source: "query Sessions {\n  sessions {\n    sessions {\n      meta {\n        id\n        project\n        slug\n        createdAt\n        updatedAt\n        messageCount\n        firstMessage\n        projectPath\n      }\n    }\n    total\n  }\n}\n\nquery Session($id: String!) {\n  session(id: $id) {\n    meta {\n      id\n      isSidechain\n      parentSessionId\n      agentId\n      firstMessage\n    }\n    events {\n      events {\n        raw\n      }\n      total\n    }\n    agentMap {\n      toolUseId\n      agentId\n    }\n  }\n}\n\nsubscription SessionEvents($id: String!) {\n  sessionEvents(id: $id) {\n    raw\n  }\n}\n\nquery SessionPage($id: String!, $page: PageInput) {\n  session(id: $id) {\n    meta {\n      id\n      filePath\n    }\n    events(page: $page) {\n      events {\n        raw\n      }\n      total\n    }\n  }\n}\n\nquery SessionMeta($id: String!) {\n  session(id: $id) {\n    meta {\n      id\n      filePath\n    }\n  }\n}\n\nquery RawLog($id: String!) {\n  session(id: $id) {\n    rawLog\n  }\n}"): typeof import('./graphql').SessionsDocument;


export function graphql(source: string) {
  return (documents as any)[source] ?? {};
}
