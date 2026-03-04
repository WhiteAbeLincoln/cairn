// AskUserQuestion tool call rendered as a question form with selectable options.
// Top-level DisplayItem kind='ask-user-question'. Also exports the inner AskUserQuestionView
// for reuse, plus the AskUserQuestion type and parseAskUserAnswers helper.

import { For } from "solid-js";
import type { DisplayableEvent, ToolResultEntry } from "../../lib/types";
import { getToolUseBlock, totalTokens, contentToString } from "../../lib/session";
import MessageBlock from "./MessageBlock";
import aq from "./AskUserQuestionView.module.css";

export function AskQuestionBlockView(props: {
  msg: DisplayableEvent;
  sessionId: string;
  toolResults: Map<string, ToolResultEntry>;
}) {
  const block = getToolUseBlock(props.msg, "AskUserQuestion")!;
  const input = block.input as { questions?: AskUserQuestion[] };
  const questions = input.questions ?? [];
  const result = props.toolResults.get(block.id);
  const answers = result
    ? parseAskUserAnswers(contentToString(result.content))
    : new Map<string, string>();
  return (
    <MessageBlock
      variant="ask-user-question"
      role="ask-user-question"
      label="Question"
      meta={{ uuid: props.msg.uuid, sessionId: props.sessionId, tokens: totalTokens(props.msg) }}
    >
      <AskUserQuestionView questions={questions} answers={answers} />
    </MessageBlock>
  );
}

export interface AskUserQuestion {
  question: string;
  header: string;
  options: { label: string; description: string }[];
  multiSelect: boolean;
}

export function parseAskUserAnswers(
  resultContent: string,
): Map<string, string> {
  const answers = new Map<string, string>();
  const regex = /"([^"]+)"="([^"]+)"/g;
  let match;
  while ((match = regex.exec(resultContent)) !== null) {
    answers.set(match[1], match[2]);
  }
  return answers;
}

export default function AskUserQuestionView(props: {
  questions: AskUserQuestion[];
  answers: Map<string, string>;
}) {
  return (
    <div class={aq["ask-questions"]} data-component="ask-user-question">
      <For each={props.questions}>
        {(q) => {
          const answer = () => props.answers.get(q.question);
          return (
            <div
              class={aq["question-group"]}
              data-question={q.header}
              itemscope
              itemtype="https://schema.org/Question"
            >
              <div class={aq["question-header"]}>
                <span class={aq["question-badge"]} itemprop="name">
                  {q.header}
                </span>
                {q.multiSelect && (
                  <span class={aq["multi-badge"]}>multi</span>
                )}
              </div>
              <div class={aq["question-text"]} itemprop="text">
                {q.question}
              </div>
              <div class={aq["question-options"]}>
                <For each={q.options}>
                  {(opt) => {
                    const selected = () => answer() === opt.label;
                    return (
                      <div
                        class={aq["question-option"]}
                        classList={{ [aq["option-selected"]]: selected() }}
                        data-selected={selected() ? "true" : undefined}
                        itemscope
                        itemtype="https://schema.org/Answer"
                        itemprop={
                          selected() ? "acceptedAnswer" : "suggestedAnswer"
                        }
                      >
                        <span class={aq["option-indicator"]}>
                          {selected() ? "\u25CF" : "\u25CB"}
                        </span>
                        <div>
                          <span class={aq["option-label"]} itemprop="text">
                            {opt.label}
                          </span>
                          <span
                            class={aq["option-desc"]}
                            itemprop="description"
                          >
                            {opt.description}
                          </span>
                        </div>
                      </div>
                    );
                  }}
                </For>
              </div>
            </div>
          );
        }}
      </For>
    </div>
  );
}
