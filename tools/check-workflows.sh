#!/bin/sh
# Checks the workflows that run the boot test (cargo xtask check):
#
# - none of them listens for the pull request `edited` event. Editing a title
#   or description changes no code, so such an edit must never start, replace
#   or cancel a boot test run;
# - the step that runs it has BISMUTH_REQUIRE_QEMU set to 1, so a missing QEMU
#   fails the check instead of ending it as "not run".
#
# There is no YAML parser, so the file is read line by line and errs on the
# side of refusing. Only a line whose first character other than a space or tab
# is `#` counts as a comment; a `#` later on a line does not hide anything.
#
# A line runs the boot test if it has `xtask` and `check` separated by spaces
# or tabs, or has `xtask` not followed on that line by a task name (a command
# continued or folded onto the next line). A file with such a line must not
# mention `edited` outside comment lines.
#
# Blocks are read by indentation: each line belongs to the nearest line above
# it that is indented less, and a list item (`- `) is a block of its own. Each
# line that runs the boot test needs BISMUTH_REQUIRE_QEMU: 1 in an `env:`
# block of a block that encloses it: its step, its job or the workflow. In a
# file that runs the boot test, every `env:` must be a plain block (nothing
# after the colon but a comment), no line may be a merge key (`<<:`), and
# every line that names BISMUTH_REQUIRE_QEMU must be such an `env:` entry
# setting it to 1, "1" or '1'. Anything else fails, as not understood:
# quoted names, flow mappings, an anchor or alias on an `env:`, other values.
#
# It cannot see into anything the file only calls: a script, composite action
# or reusable workflow that runs the check or changes the variable without
# the file itself naming `xtask` or BISMUTH_REQUIRE_QEMU. It reads the text of
# a `run: |` block as if it were YAML, and a file with CRLF line ends is
# likely to be refused.
#
# Usage:
#   check-workflows.sh [directory]   (default: .github/workflows)
set -eu

dir=${1:-.github/workflows}
status=0

# Prints one error per problem in the workflow file $1, and exits with 1 if
# there is any.
check_file() {
    awk -v file="$1" '
    function open(indent, name) {
        while (depth > 0 && at[depth] >= indent) depth--
        depth++; at[depth] = indent; key[depth] = name; id[depth] = ++blocks
    }
    function problem(number, text) { problems[++count] = file ":" number ": " text }
    BEGIN { id[0] = 0 }
    /^[ \t]*(#|$)/ { next }
    /(^|[^A-Za-z0-9_])edited([^A-Za-z0-9_]|$)/ { edited = NR }
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
        if (name == "env") plain[id[depth]] = rest ~ /^env:( +#.*)? *$/
        if (name == "env" && !plain[id[depth]]) bad_env[NR] = 1
        if (name == "<<") merge[NR] = 1
        if ($0 ~ /BISMUTH_REQUIRE_QEMU/) {
            if (rest ~ /^BISMUTH_REQUIRE_QEMU: +(1|"1"|\0471\047)( +#.*)? *$/ &&
                depth > 1 && key[depth - 1] == "env")
                on[id[depth - 2]] = 1
            else
                not_understood[NR] = 1
        }
        # Every `xtask` followed by a task name other than check is removed;
        # an `xtask` left over has no task name on this line.
        named = $0; gsub(/xtask[ \t]+[A-Za-z][A-Za-z0-9-]*/, "", named)
        if ($0 ~ /xtask[ \t]+check/ || named ~ /xtask/) {
            runs++; line[runs] = NR; within[runs] = " "
            for (d = 0; d <= depth; d++) within[runs] = within[runs] id[d] " "
        }
    }
    END {
        if (!runs) exit 0
        if (edited)
            problem(edited, "listens for `edited` and runs the boot test (cargo xtask check); " \
                "move text checks that need `edited` into a workflow of their own")
        for (n in bad_env)
            problem(n, "an `env:` that is not a plain block of `NAME: value` lines is not understood")
        for (n in merge)
            problem(n, "a merge key (`<<:`) is not understood")
        for (n in not_understood)
            problem(n, "BISMUTH_REQUIRE_QEMU in a form this check does not understand; " \
                "write `BISMUTH_REQUIRE_QEMU: 1` in a plain `env:` block")
        for (r = 1; r <= runs; r++) {
            set = 0
            for (s in on) if (index(within[r], " " s " ")) set = 1
            if (!set)
                problem(line[r], "runs the boot test (cargo xtask check) without " \
                    "BISMUTH_REQUIRE_QEMU: 1 on its step, its job or the workflow; without it, " \
                    "a missing QEMU ends the check as not run instead of failing it")
        }
        for (p = 1; p <= count; p++) print "error: " problems[p]
        exit (count > 0)
    }' "$1"
}

for file in "$dir"/*.yml "$dir"/*.yaml; do
    [ -f "$file" ] || continue
    check_file "$file" >&2 || status=1
done

exit "$status"
