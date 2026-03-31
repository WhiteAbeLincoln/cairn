#!/usr/bin/env bash
# Lists top-level keys grouped by the value of the '.type' field,
# with count, percentage, and JSON types within each message type.

set -euo pipefail

fd -e jsonl . ~/.claude/projects/ --type f -x cat {} \; \
  | jq -r '(.type // "null") as $t | to_entries[] | select(.value != null) | "\($t)\t\(.key)\t\(.value | type)"' 2>/dev/null \
  | awk -F'\t' '
    {
      mtype = $1; key = $2; jtype = $3
      count[mtype, key]++
      if (!seen[mtype, key, jtype]++) {
        jtypes[mtype, key] = (jtypes[mtype, key] ? jtypes[mtype, key] "," jtype : jtype)
      }
    }
    END {
      # collect all message types
      for (combo in count) {
        split(combo, p, SUBSEP)
        if (!typeseen[p[1]]++) alltypes[++ntypes] = p[1]
      }

      for (ti = 1; ti <= ntypes; ti++) {
        t = alltypes[ti]
        # find max count for any key = message count; collect keys
        msgcount = 0; nk = 0
        for (combo in count) {
          split(combo, p, SUBSEP)
          if (p[1] == t) {
            nk++
            knames[nk] = p[2]
            kcounts[nk] = count[combo]
            if (count[combo] > msgcount) msgcount = count[combo]
          }
        }
        printf "== %s (%d) ==\n", t, msgcount
        # sort by count descending (bubble sort)
        for (ia = 1; ia <= nk; ia++)
          for (ib = ia + 1; ib <= nk; ib++)
            if (kcounts[ib] > kcounts[ia]) {
              tmp = knames[ia]; knames[ia] = knames[ib]; knames[ib] = tmp
              tmp = kcounts[ia]; kcounts[ia] = kcounts[ib]; kcounts[ib] = tmp
            }
        for (ia = 1; ia <= nk; ia++)
          printf "    %7d  %5.1f%%  %-28s  %s\n", kcounts[ia], (kcounts[ia]/msgcount)*100, knames[ia], jtypes[t, knames[ia]]
        printf "\n"
      }
    }'
