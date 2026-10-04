#!/usr/bin/env bash
set -euo pipefail

readonly script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly runner="$script_dir/gemini.sh"
readonly temp="$(mktemp -d)"
trap 'rm -rf -- "$temp"' EXIT
mkdir -p "$temp/bin" "$temp/runtime"
cat > "$temp/bin/gemini" <<'FAKE'
#!/usr/bin/env bash
for arg in "$@"; do
  [[ "$arg" == --prompt ]] && found=1
done
[[ "${found:-0}" == 1 ]] || exit 2
printf ran > "${SENTINEL:?}"
if [[ "${FAKE_GEMINI_ERROR:-0}" == 1 ]]; then
  printf '%s\n' '{"error":{"type":"FatalToolExecutionError","message":"secret","code":"tool_error"}}'
  exit 54
fi
if [[ "${FAKE_GEMINI_STDERR_ERROR:-0}" == 1 ]]; then
  printf '%s\n' '{"error":{"type":"ProviderError","message":"secret","code":173}}' >&2
  exit 173
fi
printf '%s\n' '{"response":"sentinel"}'
FAKE
chmod +x "$temp/bin/gemini"

run() {
  (
    cd -- "$1"
    RUNNER_TEMP="$temp/runtime" GEMINI_API_KEY=test SENTINEL="$temp/ran" \
      PATH="$temp/bin:$PATH" bash "$runner"
  )
}

for unsafe in .gemini .env GEMINI.md; do
  workspace="$temp/$unsafe"
  mkdir -p "$workspace"
  if [[ "$unsafe" == .gemini ]]; then
    ln -s "$workspace/missing" "$workspace/$unsafe"
  else
    : > "$workspace/$unsafe"
  fi
  ! run "$workspace" > /dev/null 2>&1
  [[ ! -e "$temp/ran" ]]
done

workspace="$temp/clean"
mkdir -p "$workspace"
[[ "$(run "$workspace")" == sentinel ]]
[[ -e "$temp/ran" ]]

set +e
failure="$(FAKE_GEMINI_ERROR=1 run "$workspace" 2>&1)"
status=$?
set -e
[[ "$status" == 54 ]]
[[ "$failure" == *'Gemini CLI error: FatalToolExecutionError'* ]]
[[ "$failure" == *'Gemini CLI exited with status 54'* ]]
[[ "$failure" != *secret* ]]

set +e
failure="$(FAKE_GEMINI_STDERR_ERROR=1 run "$workspace" 2>&1)"
status=$?
set -e
[[ "$status" == 173 ]]
[[ "$failure" == *'Gemini CLI error: ProviderError'* ]]
