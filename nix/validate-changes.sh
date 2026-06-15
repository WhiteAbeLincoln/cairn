#!/usr/bin/env bash
LOGFILE=""
ALL=false
FIX=false
HOOKS=false
TEE=false
ARGS=()
FILES=()
while [ $# -gt 0 ]; do
  case "${1}" in
    -h|--help)
      echo "Usage: validate-changes [--all] [--fix] [--log[=FILE]] [--tee] [--hooks] [prek args] [-- file ...]"
      echo ""
      echo "  --all          Run hooks against all files (ignores git state)"
      echo "  --fix          Run formatters in write mode (fix in place)"
      echo "  --log[=FILE]   Send all output to FILE (or a new temp file if omitted)"
      echo "  --tee          With --log, tee output to both terminal and log file"
      echo "  --hooks        List available hooks and exit"
      echo "  -- file ...    Run hooks on these specific files only"
      echo ""
      echo "By default, runs hooks against files changed since HEAD."
      echo ""
      prek run --help
      exit 0
      ;;
    --hooks)
      HOOKS=true
      shift
      ;;
    --fix)
      FIX=true
      export VALIDATE_FIX=1
      shift
      ;;
    --tee)
      TEE=true
      shift
      ;;
    --log=*)
      LOGFILE="${1#--log=}"
      shift
      ;;
    --log)
      # Hardcoded /tmp — $TMPDIR inside nix develop points to an ephemeral
      # nix-shell temp dir that may be cleaned up on exit.
      LOGFILE="$(mktemp /tmp/validate-changes.XXXXXX.log)"
      shift
      ;;
    --all)
      ALL=true
      shift
      ;;
    --)
      shift
      FILES+=("$@")
      break
      ;;
    *)
      ARGS+=("${1}")
      shift
      ;;
  esac
done

if [ "$HOOKS" = true ]; then
  prek list
  exit 0
fi

FORMATTER_HOOKS=(biome cargo-fmt)

run_prek() {
  local args=("${ARGS[@]}")
  if [ "$FIX" = true ]; then
    args+=("${FORMATTER_HOOKS[@]}")
  fi

  if [ ${#FILES[@]} -gt 0 ]; then
    prek run "${args[@]}" --files "${FILES[@]}"
  elif [ "$ALL" = true ]; then
    prek run --all-files "${args[@]}"
  else
    git diff -z --name-only HEAD | xargs -0 prek run "${args[@]}" --files
  fi
}

if [ -n "$LOGFILE" ]; then
  echo "LOGGING TO $LOGFILE"
  if [ "$TEE" = true ]; then
    run_prek 2>&1 | tee "$LOGFILE"
    RC=${PIPESTATUS[0]}
  else
    run_prek > "$LOGFILE" 2>&1
    RC=$?
  fi
  if [ "$RC" -ne 0 ]; then
    echo "Validation failed — see $LOGFILE"
  else
    echo "Validation passed — see $LOGFILE"
  fi
  exit "$RC"
else
  run_prek
fi
