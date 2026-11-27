#!/bin/sh
# Checks that commits and the pull request are the repository owner's own
# work, with no AI credit lines.
#
# Environment:
#   RANGE       commits to check, as a git revision range (default: HEAD)
#   REPO_OWNER  GitHub login every commit must be authored by (optional)
#   PR_AUTHOR   GitHub login that opened the pull request (optional)
#   PR_TITLE    pull request title (optional)
#   PR_BODY     pull request description (optional)
#
# Commit authors are matched against the owner's GitHub no-reply address, so
# a commit made under a personal email address also fails. That keeps private
# addresses out of a public history.
set -eu

here=$(dirname "$0")
range=${RANGE:-HEAD}
owner=$(printf '%s' "${REPO_OWNER:-}" | tr '[:upper:]' '[:lower:]')
status=0

for commit in $(git rev-list "$range"); do
    if ! git show -s --format='%B%n%an%n%ae%n%cn%n%ce' "$commit" \
        | "$here/check-attribution.sh"; then
        echo "error: commit $commit credits an AI tool" >&2
        status=1
    fi
    if [ -n "$owner" ]; then
        email=$(git show -s --format='%ae' "$commit" | tr '[:upper:]' '[:lower:]')
        case "$email" in
            "$owner@users.noreply.github.com" | *"+$owner@users.noreply.github.com") ;;
            *)
                echo "error: commit $commit is not authored by $owner's GitHub no-reply address" >&2
                status=1
                ;;
        esac
    fi
done

if [ -n "${PR_TITLE:-}${PR_BODY:-}" ]; then
    if ! printf '%s\n%s\n' "${PR_TITLE:-}" "${PR_BODY:-}" | "$here/check-attribution.sh"; then
        echo "error: the pull request title or description credits an AI tool" >&2
        status=1
    fi
fi

if [ -n "$owner" ] && [ -n "${PR_AUTHOR:-}" ]; then
    author=$(printf '%s' "$PR_AUTHOR" | tr '[:upper:]' '[:lower:]')
    if [ "$author" != "$owner" ]; then
        echo "error: the pull request was opened by $PR_AUTHOR, not $owner" >&2
        status=1
    fi
fi

exit "$status"
