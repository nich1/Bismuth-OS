# Working on Bismuth

Bismuth is an original operating system written in Rust. Read
[docs/decisions.md](docs/decisions.md) before changing anything: it records
what has been decided and why. Each subsystem has a plain-language explainer
in `docs/`.

## Commands

| Command | What it does |
|---|---|
| `cargo xtask build` | Builds the boot image into `target/esp` |
| `cargo xtask run` | Boots it in QEMU with a window and serial output |
| `cargo xtask test` | Boots it in QEMU with no window and checks the output |
| `cargo xtask check` | Everything the pull request check runs |

Once per clone, turn on the commit hook:

```sh
git config core.hooksPath .githooks
```

## Rules

**Authorship.** Commits and pull requests are made as the repository owner.
Never add a credit line for an AI tool anywhere: no co-author trailers, no
"generated with" lines, no session links, in commit messages, pull request
titles or descriptions, code comments or documents. The commit hook and the
pull request check reject them.

**Original code.** Do not copy code from Linux, Wine, ReactOS or any other
operating system. Never read Microsoft source code, including leaked source
and the Windows Research Kernel. Work from public specifications and
documentation, and name the specification section in a comment where a
layout or protocol comes from one.

**Dependencies.** The project has none. Adding one needs the owner's approval
first, with the reason and the licence stated.

**Unsafe code.** Keep `unsafe` in small, named modules. Every `unsafe` block
carries a `SAFETY:` comment saying why it is sound, and every `unsafe fn`
documents its contract. Kernel code must return an error on bad input, not
panic.

**Verification.** Every change passes `cargo xtask check`. New behaviour
comes with a test that would fail without it. Report what was run and what
was not; never describe an unverified change as working.

**Decisions.** Architecture decisions belong to the owner. Do not change an
entry in `docs/decisions.md`, or build something that contradicts one,
without their agreement. Bring a new decision to them as a short
plain-language explainer with the options and a recommendation.

**Documentation.** When a subsystem is added or changes shape, add or update
its explainer in `docs/`. Write for a software engineer who is new to
operating systems.

## Pull request workflow

1. **Inspect.** Check the current branch, remotes, working tree and recent
   commits. Fetch before choosing a base. Never reset, stash, discard or
   overwrite changes you did not make.
2. **Branch.** Work on a descriptive feature branch cut from the current
   base, such as `fix/serial-newlines`. Keep unrelated changes out.
3. **Implement and verify.** Make the smallest complete change. Add a
   regression test for a bug. Review the final diff for accidental changes,
   secrets and generated files.
4. **Commit.** Stage explicit files. Inspect the staged diff. Write a concise
   message that says what behaviour changed. Check the whole branch against
   the base, not only the last commit.
5. **Open the pull request.** Push the branch and open the pull request with
   whatever authenticated route the environment has: a GitHub connector,
   then `gh`, then the GitHub API using the credentials git already holds.
   Never print credentials. Check first that no pull request exists for the
   branch. Open it as a draft if work or verification is incomplete.
6. **Describe it.** Lead with the concrete problem and the resulting
   behaviour. Then the validation that was run, then any remaining
   limitation. Leave out conversation history and abandoned approaches.
7. **Follow through.** Check the pull request's checks and fix failures the
   change caused. Do not force-push. Do not push to `main`, merge, or enable
   auto-merge unless the owner has said to.
