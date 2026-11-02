#!/usr/bin/env bash
# Single owner of reading a work-item key out of an intake `source:` value, and of
# the PR title rule a ship brief hands the worker for it.
# Sourced by bin/fm-brief.sh.
# fm_pr_link_key <source> prints the key and exits 0 for a Jira browse URL
# (`DPES-1234`) or an Azure DevOps work item URL (`AB#12345`, the id form Azure
# Boards links from a GitHub title); every other source, including a GitHub Issue,
# `pasted`, or an unfilled placeholder, prints nothing and exits 1.
# fm_pr_link_intake_source <intake-body> prints the value of the body's `source:` line.
# fm_pr_link_title_rule <key> prints the brief line that tells the worker how to
# title its PR.
# fm_pr_link_body_rule <source> <key> prints the brief lines that tell the worker
# how to fill the PR body's work-item block, which pr-validate reads for the
# provider, id, and url that make the title key required.
# fm_pr_link_branch_rule_ok <branch> returns 0 when pr-validate accepts <branch> as a
# Jira-keyed ship branch, else 1.
# The title is `type(scope): <key> - Description`, the Conventional Commits form
# with the key that the org's pr-validate enforces, not a bare `<key>: Description`.
# No tracker is contacted: the source string is the only input.

fm_pr_link_key() {  # <source>
  local source=${1-}
  if [[ "$source" =~ ^https://[A-Za-z0-9.-]+\.atlassian\.net/browse/([A-Z][A-Z0-9]*-[0-9]+)([^A-Za-z0-9]|$) ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
  elif [[ "$source" =~ ^https://dev\.azure\.com/[^/]+/[^/]+/_workitems/edit/([0-9]+)([^0-9]|$) ]]; then
    printf 'AB#%s\n' "${BASH_REMATCH[1]}"
  else
    return 1
  fi
}

fm_pr_link_intake_source() {  # <intake-body>
  local line value
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      source:*)
        value=${line#source:}
        value=${value#"${value%%[![:space:]]*}"}
        value=${value%"${value##*[![:space:]]}"}
        printf '%s\n' "$value"
        return 0
        ;;
    esac
  done <<<"${1-}"
  return 1
}

fm_pr_link_title_rule() {  # <key>
  printf '%s\n' \
    "This task's source work item is \`$1\`, so your PR title and your commit subject must carry that key." \
    "Write them as \`type(scope): $1 - Description\`, never \`$1: Description\`; the org's pr-validate rejects the second form."
}

fm_pr_link_body_rule() {  # <source> <key>
  local source=$1 key=$2 provider id
  case "$key" in
    AB#*) provider=azure_devops; id=${key#AB#} ;;
    *) provider=jira; id=$key ;;
  esac
  printf '%s\n' \
    "The PR body must name the same work item, because pr-validate reads that block to decide whether the title key is required." \
    "Under \`## Ticket\`, paste the link \`$source\`." \
    "Between the lines \`<!-- ai-pr-review:work-items:start -->\` and \`<!-- ai-pr-review:work-items:end -->\`, put a yaml fence with \`primary:\` and, indented under it, \`provider: $provider\`, \`id: $id\`, and \`url: $source\`."
}

# The branch form the org's pr-validate accepts for a Jira-keyed ship branch:
# type/JIRA-KEY_slug, with an underscore before the slug. It mirrors
# validate-branch.js in mc-innersource/mc-ci-reusable-workflows at action v1.4.3,
# and it covers only the keyed form, not the exempt form.
fm_pr_link_branch_rule_ok() {  # <branch>
  local LC_ALL=C re='^(feat|fix|update|docs|style|refactor|perf|test|build|ci|chore|upgrade|revert)/[A-Z][A-Z0-9]+-[0-9]+_[a-z0-9][a-z0-9-]*$'
  [[ "${1-}" =~ $re ]]
}
