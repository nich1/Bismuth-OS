#!/bin/sh
# Checks that names follow the Conventional Commits types, so the kind of
# change is visible at a glance. See docs/workflow.md.
#
# Usage:
#   check-names.sh --subject    check one commit subject read from standard input
#   check-names.sh              check whatever these variables name:
#
#     RANGE        commits whose subjects to check, as a git revision range
#     HEAD_BRANCH  branch name to check
#     PR_TITLE     pull request title to check
set -eu

types='feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert'
subject_pattern="^($types)(\\([a-z0-9._/-]+\\))?!?: [^ ].*\$"
branch_pattern="^($types)/[a-z0-9][a-z0-9._-]*\$"

# Succeeds if the text is a conventional subject. Merge commits written by
# git or the code host are let through.
is_subject() {
    printf '%s\n' "$1" | grep -Eq "$subject_pattern" && return 0
    printf '%s\n' "$1" | grep -Eq '^Merge ' && return 0
    return 1
}

explain() {
    echo "       expected \"type: description\" or \"type(scope): description\"" >&2
    echo "       types: $(printf '%s' "$types" | tr '|' ' ')" >&2
}

if [ "${1:-}" = "--subject" ]; then
    IFS= read -r subject || true
    if ! is_subject "$subject"; then
        echo "error: commit subject does not follow the convention: $subject" >&2
        explain
        exit 1
    fi
    exit 0
fi

status=0

if [ -n "${RANGE:-}" ]; then
    for commit in $(git rev-list --no-merges "$RANGE"); do
        subject=$(git show -s --format='%s' "$commit")
        if ! is_subject "$subject"; then
            echo "error: commit $commit subject does not follow the convention: $subject" >&2
            status=1
        fi
    done
fi

if [ -n "${PR_TITLE:-}" ] && ! is_subject "$PR_TITLE"; then
    echo "error: pull request title does not follow the convention: $PR_TITLE" >&2
    status=1
fi

if [ "$status" -ne 0 ]; then
    explain
fi

if [ -n "${HEAD_BRANCH:-}" ] \
    && ! printf '%s\n' "$HEAD_BRANCH" | grep -Eq "$branch_pattern"; then
    echo "error: branch name does not follow the convention: $HEAD_BRANCH" >&2
    echo "       expected \"type/short-description\" in lower case, such as feat/serial-driver" >&2
    status=1
fi

exit "$status"
