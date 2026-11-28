---
name: task
description: Take one issue from the backlog through plan, implementation, independent verification and a pull request. Use when asked to work on an issue or "the next task".
---

# Carry one task to a pull request

Do the steps in order. Do not start a step until the check for the previous
one holds.

1. **Pick up the task.** `cargo xtask issue-read <number>`. If it has a
   resume note or an open branch, use the `handoff` skill first.
   *Check:* you can state the task and its "done when" line in one sentence
   each.

2. **Read before planning.** `docs/map.md`, then the entries in
   `docs/decisions.md` the task touches.
   *Check:* you know which files will change.

3. **Plan on the issue.** A few lines in a file: approach, the test that
   will prove it, what could go wrong, and the base branch if it is not
   `main`. `cargo xtask issue-comment <number> --body-file <path>`.
   *Check:* the plan needs no architecture decision. If it does, stop and
   bring the owner a short explainer with options and a recommendation.

4. **Branch.** From the latest base branch (`main` unless the task is stacked
   on an open pull request), named `type/short-description`.
   *Check:* `git status` shows the new branch and a clean tree.

5. **Implement.** Hand the task to the `implementer` agent with the task
   statement, the "done when" line and the files. If the change adds, moves
   or removes something, the implementer updates `docs/map.md` in it.
   *Check:* its report shows the test failing before and passing after, and
   what it broke on purpose that a test noticed.

6. **Commit, then verify.** Commit the work on the branch as the `open-pr`
   skill's first two steps say. Before the first verifier pass, run every
   command the change adds or alters that reaches an outside service, such
   as the code host, for real once, not only against a stand-in: a stand-in
   once hid a permission failure that showed only in real use, after
   verification had finished. For a command that opens or changes a pull
   request, the real run is this task's own pull request: push and open it
   as a draft now. Otherwise pushing can wait. Hand the `verifier` agent the
   task statement, the "done when" line and the range `<base>...HEAD` only,
   where `<base>` is the base branch from step 4, never the working tree.
   Only *wrong behaviour* goes back to the implementer; commit its fix and
   verify once more. Never more than two passes.
   *Check:* each such command has run for real once, no wrong behaviour is
   left, and every other finding is noted for the pull request's "Not
   verified, and remaining limitations" section.

7. **Document.** If something was added, moved or removed, hand the
   `explainer` agent the change. It updates the explainers in `docs/` and
   checks the map's rows for accuracy and plain wording.
   *Check:* `cargo xtask check` passes. For the map it catches only a row
   naming a path that does not exist, so check by eye that every addition
   has a row.

8. **Open the pull request.** Use the `open-pr` skill. If step 6 opened a
   draft, do not open another: push the commits made since then with
   `git push origin refs/heads/<branch>:refs/heads/<branch>`, then replace the description with
   `cargo xtask pr-edit`. The draft is marked ready only now, by that
   skill's last step.
   *Check:* the pull request exists, its head is the branch's last commit,
   and its checks are running.

9. **Follow through.** Use the `verify` skill's waiting step. Fix failures
   the change caused.
   *Check:* every check passes, or the owner has been told exactly which one
   does not and why.

10. **After the merge.** Confirm the issue closed and the checks pass on
    `main`. Record the *untested or unguarded* findings as a comment on the
    next task's issue with `cargo xtask issue-comment`.
    *Check:* the issue is closed, the checks pass on `main`, and the next
    task's issue carries those findings.

## When a step fails

| Situation | Do this |
|---|---|
| The "done when" line cannot be met | Stop. Say why on the issue and ask the owner |
| The verifier and implementer disagree | Reproduce it yourself. A finding you can reproduce stands |
| Wrong behaviour is left after the second pass | Stop. Tell the owner what is wrong and how to reproduce it |
| A finding is *untested or unguarded*, or an *observation* | List it in the pull request. After the merge, record an *untested or unguarded* one on the next task's issue. Do not send the work back for it |
| `cargo xtask check` fails for a reason unrelated to the task | File a `papercut` issue, then fix it in its own pull request |
| The session must end mid-task | Use the `handoff` skill to leave a resume note |
| A rule or document is wrong or unclear | Use the `papercut` skill |
