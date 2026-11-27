---
name: open-pr
description: Commit the intended work, push the branch and open a pull request as the repository owner. Use when a change is verified and ready for review.
---

# Open a pull request

1. **Stage only the task.** Add files by name: the work, before it is
   verified, then here anything since, such as documents. Read the staged
   diff.
   *Check:* `git diff --cached --stat` lists nothing unrelated, no secrets and
   no build output.

2. **Commit.** Subject `type: description`. Say what behaviour changed.
   *Check:* the commit hook accepted it, and `git log main..HEAD` shows the
   owner as author of every commit.

3. **Check the whole branch, not just the last commit.**
   `git diff --stat origin/main...HEAD`.
   *Check:* every file listed belongs to the task.

4. **Push.** `git push -u origin HEAD`.
   *Check:* the remote branch exists at your commit.

5. **Make sure no pull request already exists for the branch.**
   *Check:* a search for the branch returns none. If one exists, update it.

6. **Write the description from the template.** Lead with the problem and the
   result. Then the evidence from the `verify` skill. Then what it adds to
   the system. Then what was not verified. Put multi-line text in a file.
   *Check:* `tools/check-attribution.sh` accepts the title and description.

7. **Open it** with the first route that is available: a connector for the
   code host, then `gh`, then the host's API using the credentials git
   already holds. Open it as a draft if any required verification has not run
   yet.
   *Check:* the pull request's author is the owner.

8. **When every check passes, mark a draft ready for review.** Do not merge.
   *Check:* the description states the final results.

## When a step fails

| Situation | Do this |
|---|---|
| The hook rejects the commit | Fix the message. Never bypass the hook |
| The push is rejected | Fetch and bring the branch up to date. Never force-push |
| No authenticated route works | Stop and tell the owner. Never print or copy a credential |
| A check fails | Read the failing step's log, fix the cause, push again |

Never push to `main`, merge, or enable auto-merge unless the owner has said
to.
