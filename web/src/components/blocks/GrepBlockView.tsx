// Grep tool block with syntax-highlighted content and line focus.
// Dispatched from ContentBlockView when block.name === 'Grep'.

import { createResource, Show } from "solid-js";
import { fileExtToLang, highlight } from "../../lib/highlight";
import { truncate } from "../../lib/format";
import { contentToString } from "../../lib/session";
import type { ToolResultEntry } from "../../lib/types";
import CollapsibleBlock from "./CollapsibleBlock";
import styles from "../SessionView.module.css";

export default function GrepBlockView(props: {
  blockKey: string;
  input: unknown;
  result: ToolResultEntry | undefined;
  sessionId: string;
  uuid: string;
  expanded: Set<string>;
  toggle: (key: string) => void;
  tokens?: number;
}) {
  const input = props.input as {
    path?: string;
    pattern?: string;
  };
  const filePath = input.path ?? "";
  const lang = fileExtToLang(filePath);
  const content = "";

  const [html] = createResource(
    () =>
      props.expanded.has(props.blockKey) && lang
        ? content
        : null,
    async (code) => {
      return highlight(code, lang!);
    },
  );

  return (
    <CollapsibleBlock
      role="tool"
      meta={{ sessionId: props.sessionId, uuid: props.uuid, tokens: props.tokens, result: props.result }}
      class={styles["tool-block"]}
      expanded={props.expanded.has(props.blockKey)}
      toggle={() => props.toggle(props.blockKey)}
      label={
        <>
          <span class={styles.step}>Grep</span>
          <Show when={filePath}>
            <span class={styles["tool-filepath"]}>{filePath}</span>
          </Show>
        </>
      }
    >
      <div class={styles["tool-details"]}>
        <Show
          when={html()}
          fallback={
            <pre class={styles["highlighted-code"]}>
              {truncate(content, 5000)}
            </pre>
          }
        >
          {(h) => <div class={styles["highlighted-code"]} innerHTML={h()} />}
        </Show>
        <Show when={props.result?.is_error}>
          {(_) => (
            <div class={styles["tool-section"]}>
              <div class={styles["tool-section-label"]}>Output</div>
              <pre class={styles["is-error"]}>
                {truncate(contentToString(props.result!.content), 5000)}
              </pre>
            </div>
          )}
        </Show>
      </div>
    </CollapsibleBlock>
  );
}
