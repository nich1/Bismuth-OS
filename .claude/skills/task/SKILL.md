---
name: task
description: Take one issue from the backlog through plan, implementation, independent verification and a pull request. Use when asked to work on an issue or "the next task".
---

# Carry one task to a pull request

Do the steps in order. Do not start a step until the check for the previous
one holds.

1. **Pick up the task.** Read the issue. If it has a resume note or an open
   branch, use the `handoff` skill first.
   *Check:* you can state the task and its "done when" line in one sentence
   each.

2. **Read before planning.** `docs/map.md`, then the entries in
   `docs/decisions.md` the task touches.
   *Check:* you know which files will change.

3. **Plan on the issue.** A few lines: approach, the test that will prove it,
   what could go wrong.
   *Check:* the plan needs no architecture decision. If it does, stop and
   bring the owner a short explainer with options and a recommendation.

4. **Branch.** From the latest `main`, named `type/short-description`.
   *Check:* `git status` shows the new branch and a clean tree.

5. **Implement.** Hand the task to the `implementer` agent with the task
   statement, the "done when" line and the files.
   *Check:* its report shows the test failing before and passing after.

6. **Verify.** Hand the `verifier` agent the task statement and the diff
   only. Fix what it confirms, then verify again.
   *Check:* the verifier has no confirmed findings left.

7. **Document.** If something was added or moved, hand the `explainer` agent
   the change.
   *Check:* `cargo xtask check` passes, which includes the feature map.

8. **Open the pull request.** Use the `open-pr` skill.
   *Check:* the pull request exists and its checks are running.

9. **Follow through.** Use the `verify` skill's waiting step. Fix failures
   the change caused.
   *Check:* every check passes, or the owner has been told exactly which one
   does not and why.

## When a step fails

| Situation | Do this |
|---|---|
| The "done when" line cannot be met | Stop. Say why on the issue and ask the owner |
| The verifier and implementer disagree | Reproduce it yourself. A finding you can reproduce stands |
| `cargo xtask check` fails for a reason unrelated to the task | File a `papercut` issue, then fix it in its own pull request |
| The session must end mid-task | Use the `handoff` skill to leave a resume note |
| A rule or document is wrong or unclear | Use the `papercut` skill |
