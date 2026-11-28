---
name: handoff
description: Leave a resume note when stopping mid-task, or pick a task back up from one. Use when a session is about to end with work unfinished, or when starting on an issue that already has a branch.
---

# Hand work over between sessions

A session can end at any moment, and the next one remembers nothing. What is
not written down gets redone or guessed.

## Stopping mid-task

1. **Commit what is sound.** Work in progress goes on the task's branch, not
   left uncommitted.
   *Check:* `git status` is clean, or what remains is deliberately discarded.

2. **Write the resume note as a comment on the issue:**
   - what is finished, with the branch name;
   - the base branch, if it is not `main`;
   - the next concrete step;
   - anything learned that is not yet in the repository;
   - anything tried that did not work, and why.
   *Check:* someone with only the note and the branch could continue.

3. **Push the branch.**
   *Check:* the remote branch has your last commit.

## Picking up

1. **Read the issue and its latest resume note.**
2. **Look at the branch and any open pull request.** `git log <base>..branch`,
   where `<base>` is the base branch named in the plan comment or resume note
   on the issue, or `main` if neither names one. The pull request always
   shows `main`, so do not read the base from it. Then the diff, the checks.
   *Check:* you can say what is done and what is next without rerunning
   finished work.
3. **Confirm the starting point.** Run `cargo xtask check` on the branch.
   *Check:* its result matches what the note claims. If it does not, trust
   the result, not the note.

## When a step fails

| Situation | Do this |
|---|---|
| No resume note, but a branch exists | Reconstruct from the commits and the diff, then write the note you wish you had found |
| The note and the code disagree | The code is the truth. Correct the note |
| The branch is far behind its base branch | Bring it up to date before continuing. Never force-push |
