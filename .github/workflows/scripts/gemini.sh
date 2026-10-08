#!/usr/bin/env bash
set -euo pipefail

readonly script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly policy="$script_dir/policy.toml"
readonly workspace="$PWD"
[[ -r "$policy" && -n "${GEMINI_API_KEY:-}" ]] || exit 1
compgen -G '/etc/gemini-cli/policies/*.toml' > /dev/null && exit 1
[[ -d "$workspace" ]] || exit 1
for unsafe in .gemini .env GEMINI.md; do
  unsafe_path="$workspace/$unsafe"
  if [[ -e "$unsafe_path" || -L "$unsafe_path" ]]; then
    printf 'Drukal stopped: hosted editing will not load repository-controlled Gemini configuration: %s\n' "$unsafe" >&2
    exit 1
  fi
done
cd -- "$workspace"

export GEMINI_CLI_HOME="${RUNNER_TEMP:?}/drukal-gemini"

approval_mode=auto_edit
if [[ "${DRUKAL_READ_ONLY:-0}" == 1 ]]; then
  approval_mode=plan
fi

arguments=(--skip-trust --approval-mode "$approval_mode" --admin-policy "$policy" --output-format json --prompt '')
if [[ -n "${DRUKAL_MODEL:-}" ]]; then
  arguments+=(--model "$DRUKAL_MODEL")
fi
readonly stderr_file="$(mktemp "${RUNNER_TEMP}/drukal-gemini-stderr.XXXXXX")"
trap 'rm -f -- "$stderr_file"' EXIT
set +e
gemini "${arguments[@]}" 2> "$stderr_file" | jq -er '
  if .error then
    (.error.type | if type == "string" and test("^[A-Za-z][A-Za-z0-9_-]{0,63}$") then . else "unknown" end) as $kind
    | ("Gemini CLI error: " + $kind + "\n" | halt_error(1))
  else .response | strings end
'
statuses=("${PIPESTATUS[@]}")
set -e
cat -- "$stderr_file" >&2
if (( statuses[0] != 0 )); then
  kind="$(jq -er '.error.type | select(type == "string" and test("^[A-Za-z][A-Za-z0-9_-]{0,63}$"))' "$stderr_file" 2>/dev/null || true)"
  if [[ -n "$kind" ]]; then
    printf 'Gemini CLI error: %s\n' "$kind" >&2
  fi
  printf 'Gemini CLI exited with status %s\n' "${statuses[0]}" >&2
  exit "${statuses[0]}"
fi
exit "${statuses[1]}"
