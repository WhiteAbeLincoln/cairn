#!/usr/bin/env bash
# Reads "session_uuid message_uuid" pairs from stdin, finds the session JSONL
# under ~/.claude/projects/, and outputs {"message": ..., "children": [...]}
# for each pair.

set -euo pipefail

PROJECTS_DIR="${CLAUDE_PROJECTS_DIR:-$HOME/.claude/projects}"

while read -r session_uuid message_uuid; do
  [[ -z "$session_uuid" || -z "$message_uuid" ]] && continue

  session_file=$(fd --type f "${session_uuid}.jsonl" "$PROJECTS_DIR" | head -1)
  if [[ -z "$session_file" ]]; then
    echo "session not found: $session_uuid" >&2
    continue
  fi

  jq -c --arg mid "$message_uuid" '
    select(.uuid == $mid or (.type == "user" and .parentUuid == $mid))
  ' "$session_file" \
  | jq -s --arg mid "$message_uuid" '
    {
      message: (map(select(.uuid == $mid)) | first),
      children: [.[] | select(.parentUuid == $mid)]
    }
  '
done
