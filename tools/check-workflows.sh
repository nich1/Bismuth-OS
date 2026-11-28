#!/bin/sh
# Checks the workflows that run the boot test (cargo xtask check):
#
# - none of them listens for the pull request `edited` event. Editing a title
#   or description changes no code, so such an edit must never start, replace
#   or cancel a boot test run;
# - each sets BISMUTH_REQUIRE_QEMU to 1, so a missing QEMU fails the check
#   instead of ending it as "not run".
#
# Usage:
#   check-workflows.sh [directory]   (default: .github/workflows)
set -eu

dir=${1:-.github/workflows}
status=0

for file in "$dir"/*.yml "$dir"/*.yaml; do
    [ -f "$file" ] || continue
    # Comment lines are ignored, so a note that mentions any of these is fine.
    grep -Eq '^[^#]*cargo xtask check' "$file" || continue
    if grep -Eq '^[^#]*\bedited\b' "$file"; then
        echo "error: $file listens for \`edited\` and runs the boot test (cargo xtask check)" >&2
        echo "       move text checks that need \`edited\` into a workflow of their own" >&2
        status=1
    fi
    if ! grep -Eq "^[^#]*BISMUTH_REQUIRE_QEMU: *[\"']?1[\"']? *(#.*)?$" "$file"; then
        echo "error: $file runs the boot test (cargo xtask check) without BISMUTH_REQUIRE_QEMU: 1" >&2
        echo "       without it, a missing QEMU ends the check as not run instead of failing it" >&2
        status=1
    fi
done

exit "$status"
