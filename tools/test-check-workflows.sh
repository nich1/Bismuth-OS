#!/bin/sh
# Tests tools/check-workflows.sh: runs it on sample workflow files, one per
# folder, and on the real ones, and fails if any verdict is wrong.
#
# Usage:
#   test-check-workflows.sh
set -eu

here=$(dirname "$0")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0

# expect <pass|fail> <name>: the workflow text is read from standard input.
expect() {
    mkdir "$work/$2"
    cat > "$work/$2/ci.yml"
    if sh "$here/check-workflows.sh" "$work/$2" 2> "$work/$2.err"; then
        verdict=pass
    else
        verdict=fail
    fi
    if [ "$verdict" != "$1" ]; then
        echo "error: expected $1 for $2, got $verdict" >&2
        sed 's/^/       /' "$work/$2.err" >&2
        failures=$((failures + 1))
    fi
}

expect pass on-the-step <<'EOF'
on: [push]
jobs:
  boot:
    steps:
      - uses: actions/checkout@v4
      - name: Run all checks
        env:
          BISMUTH_REQUIRE_QEMU: 1
        run: cargo xtask check
EOF

expect pass on-the-step-after-run <<'EOF'
jobs:
  boot:
    steps:
      - run: |
          cargo xtask check
        env:
          BISMUTH_REQUIRE_QEMU: "1"  # quoted, with a comment
EOF

expect pass on-the-job <<'EOF'
jobs:
  boot:
    runs-on: ubuntu-24.04
    env:
      BISMUTH_REQUIRE_QEMU: 1
    steps:
      - name: Run all checks
        run: cargo xtask check
EOF

expect pass on-the-workflow <<'EOF'
env:
  BISMUTH_REQUIRE_QEMU: '1'
jobs:
  boot:
    steps:
    - run: cargo xtask check
EOF

expect pass no-boot-test <<'EOF'
jobs:
  text:
    steps:
      - run: sh tools/check-names.sh
EOF

expect pass other-tasks-only <<'EOF'
jobs:
  build:
    steps:
      # Commented out: cargo xtask check
      - run: cargo xtask build && cargo xtask snapshot
EOF

expect fail missing <<'EOF'
jobs:
  boot:
    steps:
      - run: cargo xtask check
EOF

expect fail on-another-step <<'EOF'
jobs:
  boot:
    steps:
      - name: Install QEMU
        env:
          BISMUTH_REQUIRE_QEMU: 1
        run: sudo apt-get install -y qemu-system-x86
      - name: Run all checks
        run: cargo xtask check
EOF

expect fail on-another-job <<'EOF'
jobs:
  text:
    env:
      BISMUTH_REQUIRE_QEMU: 1
    steps:
      - run: sh tools/check-names.sh
  boot:
    steps:
      - run: cargo xtask check
EOF

expect fail on-a-step-of-another-job <<'EOF'
jobs:
  boot:
    steps:
      - run: cargo xtask check
  other:
    steps:
      - env:
          BISMUTH_REQUIRE_QEMU: 1
        run: echo
EOF

expect fail turned-off-on-the-step <<'EOF'
env:
  BISMUTH_REQUIRE_QEMU: 1
jobs:
  boot:
    steps:
      - env:
          BISMUTH_REQUIRE_QEMU: 0
        run: cargo xtask check
EOF

expect fail commented-out <<'EOF'
jobs:
  boot:
    steps:
      - env:
          # BISMUTH_REQUIRE_QEMU: 1
          OTHER: 1
        run: cargo xtask check
EOF

expect fail not-under-env <<'EOF'
jobs:
  boot:
    steps:
      - with:
          BISMUTH_REQUIRE_QEMU: 1
        run: cargo xtask check
EOF

# Not understood, so refused: the rule reads only block-style `env:` maps.
expect fail flow-style-env <<'EOF'
jobs:
  boot:
    steps:
      - env: { BISMUTH_REQUIRE_QEMU: 1 }
        run: cargo xtask check
EOF

# turned-off <name> <setting>: the workflow sets 1 and the step running the
# check turns it off in a form the rule does not read as an `env:` entry.
turned_off() {
    expect fail "$1" <<EOF
env:
  BISMUTH_REQUIRE_QEMU: 1
jobs:
  boot:
    steps:
      - run: cargo xtask check
$2
EOF
}
turned_off off-quoted-name '        env:
          "BISMUTH_REQUIRE_QEMU": 0'
turned_off off-space-before-colon '        env:
          BISMUTH_REQUIRE_QEMU : 0'
turned_off off-flow-env '        env: {BISMUTH_REQUIRE_QEMU: 0}'
turned_off off-alias-env '        env: *off'
turned_off off-merge-key '        env:
          <<: {BISMUTH_REQUIRE_QEMU: 0}'
turned_off off-merge-key-alias '        env:
          <<: *off'

# Ways of running the check that a literal match for the words misses.
expect fail two-spaces <<'EOF'
jobs:
  boot:
    steps:
      - run: cargo  xtask	check
EOF

expect fail hash-in-quotes <<'EOF'
jobs:
  boot:
    steps:
      - run: "echo '#'; cargo xtask check"
EOF

expect fail continued-line <<'EOF'
jobs:
  boot:
    steps:
      - run: |
          cargo xtask \
            check
EOF

expect fail folded-plain-scalar <<'EOF'
jobs:
  boot:
    steps:
      - run: cargo xtask
          check
EOF

expect fail listens-for-edited-after-a-hash-in-quotes <<'EOF'
on:
  pull_request:
    types: ["#", edited]
env:
  BISMUTH_REQUIRE_QEMU: 1
jobs:
  boot:
    steps:
      - run: cargo xtask check
EOF

expect fail listens-for-edited <<'EOF'
on:
  pull_request:
    types: [opened, edited]
env:
  BISMUTH_REQUIRE_QEMU: 1
jobs:
  boot:
    steps:
      - run: cargo xtask check
EOF

if ! sh "$here/check-workflows.sh" "$here/../.github/workflows"; then
    echo "error: the real workflows fail the check" >&2
    failures=$((failures + 1))
fi

if [ "$failures" -ne 0 ]; then
    echo "error: $failures workflow check test(s) failed" >&2
    exit 1
fi
echo "workflow check tests passed"
