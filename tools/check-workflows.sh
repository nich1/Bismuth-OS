#!/bin/sh
# Checks that no workflow both listens for the pull request `edited` event and
# runs the boot test. Editing a title or description changes no code, so such
# an edit must never start, replace or cancel a boot test run.
#
# Usage:
#   check-workflows.sh [directory]   (default: .github/workflows)
set -eu

dir=${1:-.github/workflows}
status=0

for file in "$dir"/*.yml "$dir"/*.yaml; do
    [ -f "$file" ] || continue
    # Comment lines are ignored, so a note that mentions either is fine.
    if grep -Eq '^[^#]*\bedited\b' "$file" \
        && grep -Eq '^[^#]*cargo xtask check' "$file"; then
        echo "error: $file listens for \`edited\` and runs the boot test (cargo xtask check)" >&2
        echo "       move text checks that need \`edited\` into a workflow of their own" >&2
        status=1
    fi
done

exit "$status"
