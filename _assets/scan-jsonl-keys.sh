#!/usr/bin/env bash
# Scans all .jsonl files in ~/.claude/projects and prints
# top-level JSON keys with their frequency and percentage, sorted descending.

set -euo pipefail

total=$(fd -e jsonl . ~/.claude/projects/ --type f -x cat {} \; | wc -l)

fd -e jsonl . ~/.claude/projects/ --type f -x cat {} \; \
  | jq -r '[to_entries[] | select(.value != null) | .key] | .[]' 2>/dev/null \
  | sort \
  | uniq -c \
  | sort -rn \
  | awk -v total="$total" '{ printf "%7d  %5.1f%%  %s\n", $1, ($1/total)*100, $2 }'
