---
name: papercut
description: Report something in the tooling, rules or documents that got in the way. Use when a document is stale, a rule is unclear or contradictory, or a command fails for a reason unrelated to the task.
---

# File a papercut

A papercut is friction in how work is done, not a bug in Bismuth. Working
around one quietly means the next session hits it too.

1. **Check it is not already filed.** Search open issues labelled `papercut`.
   *Check:* no existing issue describes the same thing.

2. **File it:** `cargo xtask issue-new --title <text> --body-file <path>
   --label papercut`, the body saying:
   - what you were doing;
   - what got in the way, with the exact command and output or the exact
     sentence that was wrong;
   - what you did to get past it, if anything;
   - the highest rung of the trust ladder a fix would fit (see
     `docs/workflow.md`).
   *Check:* the issue can be understood without the session it came from.

3. **Carry on with the task.** Do not fix the papercut inside an unrelated
   pull request unless it blocks the task completely.
   *Check:* the task's diff contains only the task.

## When a step fails

| Situation | Do this |
|---|---|
| You cannot create issues from this session | Put the same text in the pull request's "not verified" section so it is not lost |
| The papercut blocks the task | Fix it first, in its own branch and pull request |
| You were corrected by the owner | That is not a papercut. Record the correction on the trust ladder |
