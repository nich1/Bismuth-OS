#!/bin/sh
# Checks the workflows that run the boot test (cargo xtask check):
#
# - none of them listens for the pull request `edited` event. Editing a title
#   or description changes no code, so such an edit must never start, replace
#   or cancel a boot test run;
# - the step that runs it has BISMUTH_REQUIRE_QEMU set to 1, so a missing QEMU
#   fails the check instead of ending it as "not run".
#
# There is no YAML parser, so the second rule reads the file by indentation:
# each line belongs to the nearest line above it that is indented less, and a
# list item (`- `) is a block of its own. A line running the check passes if
# an `env:` block on the step, its job or the workflow (one whose parent
# encloses that line) sets the variable to 1, and none of them sets it to
# anything else. It is conservative: `env: {...}`, quoted names, anchors and
# reusable workflows are not understood, so they fail. It cannot see the
# variable changed inside the command, such as `BISMUTH_REQUIRE_QEMU=0 cargo
# xtask check`, and it reads the text of a `run: |` block as if it were YAML.
#
# Usage:
#   check-workflows.sh [directory]   (default: .github/workflows)
set -eu

dir=${1:-.github/workflows}
status=0

# Prints the number of each line that runs the boot test without
# BISMUTH_REQUIRE_QEMU: 1 on its step, its job or the workflow.
unguarded_runs() {
    awk '
    function open(indent, name) {
        while (depth > 0 && at[depth] >= indent) depth--
        depth++; at[depth] = indent; key[depth] = name; id[depth] = ++blocks
    }
    BEGIN { id[0] = 0 }
    /^[ ]*(#|$)/ { next }
    {
        match($0, /^ */); indent = RLENGTH; rest = substr($0, indent + 1)
        if (rest ~ /^-( |$)/) {
            open(indent, "-")
            rest = substr(rest, 2); match(rest, /^ */)
            indent += 1 + RLENGTH; rest = substr(rest, RLENGTH + 1)
            if (rest == "") next
        }
        name = rest; sub(/:.*/, "", name)
        open(indent, name)
        if (name == "BISMUTH_REQUIRE_QEMU" && depth > 1 && key[depth - 1] == "env") {
            value = rest
            sub(/^[^:]*: */, "", value); sub(/ *(#.*)?$/, "", value)
            scope = id[depth - 2]
            if (value ~ /^(1|"1"|\0471\047)$/) on[scope] = 1
            else off[scope] = 1
        }
        if ($0 ~ /^[^#]*cargo xtask check/) {
            runs++; line[runs] = NR; within[runs] = " "
            for (d = 0; d <= depth; d++) within[runs] = within[runs] id[d] " "
        }
    }
    END {
        for (r = 1; r <= runs; r++) {
            set = 0; unset = 0
            for (s in on) if (index(within[r], " " s " ")) set = 1
            for (s in off) if (index(within[r], " " s " ")) unset = 1
            if (!set || unset) print line[r]
        }
    }' "$1"
}

for file in "$dir"/*.yml "$dir"/*.yaml; do
    [ -f "$file" ] || continue
    # Comment lines are ignored, so a note that mentions any of these is fine.
    grep -Eq '^[^#]*cargo xtask check' "$file" || continue
    if grep -Eq '^[^#]*\bedited\b' "$file"; then
        echo "error: $file listens for \`edited\` and runs the boot test (cargo xtask check)" >&2
        echo "       move text checks that need \`edited\` into a workflow of their own" >&2
        status=1
    fi
    for number in $(unguarded_runs "$file"); do
        echo "error: $file:$number runs the boot test (cargo xtask check) without BISMUTH_REQUIRE_QEMU: 1" >&2
        echo "       set it on that step, its job or the workflow; without it, a missing QEMU" >&2
        echo "       ends the check as not run instead of failing it" >&2
        status=1
    done
done

exit "$status"
