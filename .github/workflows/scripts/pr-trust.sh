#!/usr/bin/env bash
set -euo pipefail

echo 'allowed=false' >> "$GITHUB_OUTPUT"
[[ "$PR_NUMBER" =~ ^[1-9][0-9]*$ ]] || {
  echo 'PR number must be positive' >&2
  exit 1
}
pull=$(gh api "repos/$GH_REPO/pulls/$PR_NUMBER")
jq -e --arg repo "$GH_REPO" --argjson number "$PR_NUMBER" '
  .number == $number and
  .state == "open" and .draft == false and
  (.base | type == "object") and
  (.base.repo | type == "object") and
  (.base.repo.full_name | type == "string" and . == $repo) and
  (.head | type == "object") and
  (.head.sha | type == "string" and test("^[a-f0-9]{40}$"))
' <<<"$pull" > /dev/null
dependency=$(jq -r '.user.login == "dependabot[bot]"' <<<"$pull")
if [[ "$dependency" == true ]]; then
  jq -e --arg repo "$GH_REPO" '.head.repo.full_name == $repo' <<<"$pull" > /dev/null
fi
{
  echo 'allowed=true'
  echo "dependency=$dependency"
  echo "head=$(jq -r '.head.sha' <<<"$pull")"
  echo "number=$PR_NUMBER"
} >> "$GITHUB_OUTPUT"
