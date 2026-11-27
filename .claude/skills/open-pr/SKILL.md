---
name: open-pr
description: Commit the intended work, push the branch and open a pull request as the repository owner. Use when a change is verified and ready for review; the `task` skill also uses steps 1 and 2 to commit work before verifying it.
---

# Open a pull request

1. **Stage only the task.** Add files by name: anything not yet committed,
   such as documents added after verification. Read the staged diff. If
   nothing changed since the last commit, skip this step and the next.
   *Check:* `git diff --cached --stat` lists nothing unrelated, no secrets and
   no build output.

2. **Commit.** Subject `type: description`. Say what behaviour changed.
   *Check:* the commit hook accepted it, and `git log main..HEAD` shows the
   owner as author of every commit.

3. **Check the whole branch, not just the last commit.**
   `git diff --stat origin/main...HEAD`.
   *Check:* every file listed belongs to the task.

4. **Write the description from the template** in a file. Lead with the
   problem and the result. Then the evidence from the `verify` skill. Then
   what it adds to the system. Then what was not verified.
   *Check:* every section of the template is filled in.

5. **Push and open it:** `cargo xtask pr-open --title <text> --body-file
   <path>`, adding `--draft` if any required verification has not run yet.
   It refuses text that credits a tool, and never pushes `main` or forces.
   *Check:* it printed the pull request's address, and the author is the
   owner.

6. **When every check passes, mark a draft ready:** `cargo xtask pr-ready`.
   Do not merge.
   *Check:* the description states the final results.

## When a step fails

| Situation | Do this |
|---|---|
| The hook rejects the commit | Fix the message. Never bypass the hook |
| `pr-open` says the branch already has one | Update it: `cargo xtask pr-edit --body-file <path>` |
| The push is rejected | Fetch and bring the branch up to date. Never force-push |
| The command says gh has no credential | Stop and tell the owner. Never print or copy a credential |
| A check fails | Read the failing step's log, fix the cause, push again |

Never push to `main`, merge, or enable auto-merge unless the owner has said
to.
