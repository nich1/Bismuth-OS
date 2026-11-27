# How work gets done

Bismuth is built by its owner working with coding agents. This describes who
does what, the path a task takes, and the habits that keep quality up when
most of the typing is not done by a person.

## Roles

| Role | Does |
|---|---|
| Owner | Sets the order of work, makes architecture decisions, approves by merging |
| Orchestrator | Plans a task, splits it, checks the evidence, opens the pull request |
| Implementer | Makes one scoped change, test first |
| Verifier | Sees only the change and the task, and tries to break it |
| Explainer | Keeps the documents and the feature map current |
| Runner | Mechanical jobs: running checks, searching, summarising logs |

The implementer and verifier use the most capable model. The explainer and
runner use smaller ones, because their work is cheaper to check. The role
definitions are in `.claude/agents/`.

## The path of one task

1. **Backlog.** A task is an issue. It is small and has a "done when" line: a
   check that proves it is finished. The owner sets the order.
2. **Start.** A session takes the top issue and writes a short plan on it.
3. **Decision gate.** If the plan needs an architecture decision, work stops
   and the owner gets a short explainer with the options and a recommendation.
   A decision comes with a small prototype or a measurement, not only an
   argument.
4. **Implement.** On a branch named for the task. The failing test comes
   first, and the implementer breaks its own code once to see a test notice.
5. **Verify.** `cargo xtask check` passes and the work is committed, then the
   verifier attacks the commits, in its own copy, without seeing the
   implementer's reasoning. At most two passes: only wrong behaviour goes
   back; other findings are listed in the pull request.
6. **Pull request.** Opened as the owner, with the problem, the result, the
   evidence and what is left.
7. **Checks.** Tests, authorship and naming all run automatically.
8. **Owner.** Merging is the approval. A comment sends the work back.
9. **After merge.** The feature map is updated. Any correction the owner made
   climbs the trust ladder.

The procedures behind these steps are skills in `.claude/skills/`.

## The trust ladder

When an agent makes a mistake and is corrected, the correction is recorded as
high on this ladder as it will fit:

1. **Code that makes the mistake impossible.** A type, an API that cannot be
   misused.
2. **A lint, a test or a check** that fails when the mistake is made.
3. **A skill** that describes the right procedure.
4. **A written rule** in `AGENTS.md`. The last resort, because a rule only
   works if it is read and followed.

A comment that states a constraint, such as "must not be called twice", is a
sign that a rung higher up is available.

## Papercuts

When the tooling itself gets in the way (a stale document, an unclear rule, a
command that fails for no good reason), the agent files an issue labelled
`papercut` and does not quietly work around it. Each one is fixed at the
highest rung of the ladder it will fit.

## Picking work back up

A session can end at any point. Before it stops mid-task, the agent leaves a
resume note on the issue: what is done, what is next, and anything it learned
that is not yet in the repository. The next session starts from the issue,
the branch and the pull request, and does not redo finished work.

## Names

Branches, commit subjects and pull request titles follow the Conventional
Commits types, so the kind of change is visible at a glance:

| Thing | Shape | Example |
|---|---|---|
| Branch | `type/short-description` | `feat/serial-driver` |
| Commit subject and pull request title | `type: description` or `type(scope): description` | `fix(boot): send carriage return before newline` |

The types are `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`,
`build`, `ci`, `chore` and `revert`. The commit hook and the pull request
check enforce this.

## Where sessions run

| | On the owner's PC, steered remotely | On hosted machines |
|---|---|---|
| Used when | The PC is on; the default | The PC is off |
| A decision reaches the owner by | A notification on their phone | The owner checking in |
| A pull request reaches the owner by | The code host's phone app | The code host's phone app |
| Can run on real hardware | Yes | No |

## Keeping the cost down

- **Fixed sequences are scripts.** Anything done the same way every time
  becomes a `cargo xtask` command, not a series of steps an agent re-derives.
- **Little is loaded by default.** `AGENTS.md` stays short. Procedures live in
  skills, which are read only when used.
- **Waiting is done by a script,** not by an agent checking repeatedly.
