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
  (.head.repo | type == "object") and
  (.head.repo.full_name | type == "string" and . == $repo) and
  (.head.sha | type == "string" and test("^[a-f0-9]{40}$"))
' <<<"$pull" > /dev/null
dependency=$(jq -r '.user.login == "dependabot[bot]"' <<<"$pull")
release=$(jq -r --arg repo "$GH_REPO" '
  $repo == "keys-i/koelu" and
  (.user.login == "koelu[bot]" or .user.login == "app/koelu") and
  .base.ref == "main" and
  (.head.ref | startswith("release-please--branches--main--components--")) and
  (.title | test("^chore\\(main\\): release (0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)$"))
' <<<"$pull")
jq -e --argjson release "$release" '
  (.user.login == "dependabot[bot]" or
    $release or
    .author_association == "OWNER" or
    .author_association == "MEMBER" or
    .author_association == "COLLABORATOR")
' <<<"$pull" > /dev/null || {
  echo 'Koelu runs only for same-repository Dependabot, Koelu release, or trusted collaborator pull requests' >&2
  exit 0
}
{
  echo 'allowed=true'
  echo "dependency=$dependency"
  echo "head=$(jq -r '.head.sha' <<<"$pull")"
  echo "number=$PR_NUMBER"
} >> "$GITHUB_OUTPUT"
