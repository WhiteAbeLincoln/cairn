// Bash tool call rendered as a syntax-highlighted command with collapsible output.
// Top-level DisplayItem kind='bash'. Also exports the HighlightedBash component.

import { createResource, Show } from "solid-js";
import { stripAnsi } from "../../lib/format";
import type { DisplayableEvent, ToolResultEntry } from "../../lib/types";
import { getToolUseBlock, totalTokens, contentToString } from "../../lib/session";
import { highlightBash } from "../../lib/highlight";
import MessageBlock from "./MessageBlock";
import bb from "./BashBlockView.module.css";
import styles from "../SessionView.module.css";

export function HighlightedBash(props: { code: string }) {
  const [html] = createResource(
    () => props.code,
    (code) => highlightBash(code),
  );
  return (
    <Show
      when={html()}
      fallback={
        <pre class={bb["bash-command"]}>
          <code>{props.code}</code>
        </pre>
      }
    >
      {(h) => <div class={bb["bash-command"]} innerHTML={h()} />}
    </Show>
  );
}

export default function BashBlockView(props: {
  msg: DisplayableEvent;
  sessionId: string;
  toolResults: Map<string, ToolResultEntry>;
  expanded: Set<string>;
  toggle: (key: string) => void;
}) {
  const block = getToolUseBlock(props.msg, "Bash")!;
  const input = block.input as { command?: string; description?: string };
  const command = input.command ?? "";
  const description = input.description ?? "";
  const result = props.toolResults.get(block.id);
  const outputKey = `${props.msg.uuid}-bash-output`;
  return (
    <MessageBlock
      variant="bash"
      role="bash"
      label="$"
      meta={{ sessionId: props.sessionId, uuid: props.msg.uuid, tokens: totalTokens(props.msg), result }}
      extraMeta={
        <span class={bb["bash-desc"]}>{description}</span>
      }
    >
      <HighlightedBash code={command} />
      <Show when={result}>
        {(r) => (
          <div class={bb["bash-output-section"]}>
            <button
              class={styles.toggle}
              onClick={() => props.toggle(outputKey)}
            >
              {props.expanded.has(outputKey) ? "\u25BE" : "\u25B8"} Output
            </button>
            <Show when={props.expanded.has(outputKey)}>
              <pre
                class={bb["bash-output"]}
                classList={{ [styles["is-error"]]: !!r().is_error }}
              >
                {stripAnsi(contentToString(r().content))}
              </pre>
            </Show>
          </div>
        )}
      </Show>
    </MessageBlock>
  );
}
