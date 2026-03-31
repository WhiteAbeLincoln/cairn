#!/usr/bin/env bash
# Checks whether the 'uuid' field is unique across all JSONL messages.

set -euo pipefail

uuids=$(fd -e jsonl . ~/.claude/projects/ --type f -x cat {} \; \
  | jq -r '.uuid // empty' 2>/dev/null)

total=$(echo "$uuids" | wc -l | tr -d ' ')
unique=$(echo "$uuids" | sort -u | wc -l | tr -d ' ')
dupes=$((total - unique))

echo "Total:      $total"
echo "Unique:     $unique"
echo "Duplicates: $dupes"

if [ "$dupes" -gt 0 ]; then
  echo ""
  echo "Duplicate UUIDs (count | uuid):"
  echo "$uuids" | sort | uniq -cd | sort -rn | head -20
fi
