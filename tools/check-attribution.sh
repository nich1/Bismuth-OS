#!/bin/sh
# Reads text on standard input and fails if it credits an AI tool.
#
# Used by the commit-msg hook and by the pull request check, so a credit line
# cannot reach the project history by accident.
#
# The directory name `.claude` and the file name `CLAUDE.md` are allowed,
# because a commit may need to mention those files.
set -eu

text=$(tr '[:upper:]' '[:lower:]' | sed -e 's/\.claude//g' -e 's/claude\.md//g')

if printf '%s\n' "$text" | grep -Eq 'claude|anthropic|chatgpt|openai'; then
    echo "error: this text credits an AI tool; remove the credit line:" >&2
    printf '%s\n' "$text" | grep -En 'claude|anthropic|chatgpt|openai' >&2
    exit 1
fi
