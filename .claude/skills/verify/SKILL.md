---
name: verify
description: Prove a change works and collect the evidence a pull request needs. Use before opening or updating any pull request.
---

# Prove a change

"It should work" is not evidence. Evidence is a command and what it printed.

1. **Show the test fails without the change.** Run the new test against the
   code as it was; for new code, break the code instead. Keep the output.
   *Check:* the failure is the one the change is meant to fix, not a
   compile error or a typo.

2. **Show it passes with the change.**
   *Check:* the same test, the same command, now passing.

3. **Run everything.** `cargo xtask check`.
   *Check:* its last line is `check passed`. Keep the last lines of output:
   one line per step, then that one.

4. **List what the change adds to the system.** New `unsafe` blocks, system
   calls, message interfaces, components trusted to share memory with the
   core, dependencies.
   *Check:* you counted from the diff, not from memory.

5. **Write the evidence section.** Commands and output for steps 1 to 3, the
   list from step 4, and a plain statement of anything not run.
   *Check:* someone who has not seen the session could repeat each command.

6. **After the pull request is open, wait for its checks:**
   `cargo xtask pr-wait`. Do not check repeatedly by hand.
   *Check:* it printed every check's result, and for a failure, the end of
   the failing step's log.

## When a step fails

| Situation | Do this |
|---|---|
| The new test passes without the change | The test proves nothing. Rewrite it |
| QEMU is not installed | The check ends with `boot test NOT RUN: QEMU is not installed` and exits with code 3, after every other step has run. Say the boot test did not run locally. The pull request check will run it, and there a missing QEMU is a failure |
| A check fails only on the code host | Read that job's log for the failing step. Do not rerun and hope |
| A test passes sometimes | Treat it as a bug. Find the cause or delete the test |
