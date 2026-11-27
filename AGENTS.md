# Working on Bismuth

Bismuth is an original operating system written in Rust.

Read these first, in this order:

1. [docs/map.md](docs/map.md): what exists and where.
2. [docs/decisions.md](docs/decisions.md): what has been decided and why.
3. [docs/workflow.md](docs/workflow.md): the path a task takes.
4. [docs/testing.md](docs/testing.md): how changes are verified.

## Commands

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the boot image into `target/esp` |
| `cargo xtask run` | Boots it in QEMU with a window and serial output |
| `cargo xtask test` | Boots it in QEMU with no window and checks the output |
| `cargo xtask snapshot` | Builds the boot image and prints the footprint report; `--update` also stores it in `snapshots/footprint.txt` |
| `cargo xtask check` | Everything the pull request check runs |

Once per clone, turn on the commit hook:

```sh
git config core.hooksPath .githooks
```

## Rules

**Authorship.** Commits and pull requests are made as the repository owner.
Never add a credit line for a tool anywhere: no co-author trailers, no
"generated with" lines, no session links, in commit messages, pull request
titles or descriptions, code comments or documents.

**Names.** Branches are `type/short-description`. Commit subjects and pull
request titles are `type: description`. The types and examples are in
[docs/workflow.md](docs/workflow.md).

**Original code.** Do not copy code from Linux, Wine, ReactOS or any other
operating system. Never read Microsoft source code, including leaked source
and the Windows Research Kernel. Work from public specifications, and name
the specification section in a comment where a layout or protocol comes from
one.

**Dependencies.** The project has none. Adding one needs the owner's approval
first, with the reason and the licence stated.

**Unsafe code.** Keep `unsafe` in small, named modules. Every `unsafe` block
carries a `SAFETY:` comment saying why it is sound, and every `unsafe fn`
documents its contract. Kernel code returns an error on bad input; it does
not panic.

**Verification.** Every change passes `cargo xtask check` and comes with a
test that fails without it. Show what was run and what it printed. Say what
was not run. Never describe an unverified change as working.

**Decisions.** Architecture decisions belong to the owner. Do not change an
entry in `docs/decisions.md`, or build something that contradicts one,
without their agreement.

**Merging.** Do not push to `main`, merge, force-push or enable auto-merge
unless the owner has said to.

**When something gets in your way,** file an issue labelled `papercut`. Do
not work around it quietly.

**When you are corrected,** record the correction as high on the trust ladder
as it will fit. A rule in this file is the last resort.
