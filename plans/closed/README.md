# Closed plans

This directory holds plan files that left active rotation. Each one
carries a header at the top of the file announcing one of:

- `Status: Superseded` — the goal was achieved by a different
  approach. The original plan is kept because it documents the
  rejected design and, in some cases, the rollback path if the
  shipped approach later breaks.
- `Status: Abandoned` — the goal itself was withdrawn. Kept as a
  record of decisions-not-taken.
- `Status: Completed` — every task ticked, archived for history.

Files here are **not** garbage. They are the project's institutional
record of approaches considered and rejected — useful when a future
contributor proposes the same idea, or when the shipped approach
fails and a previously-rejected plan needs to be reactivated.

When closing a plan, prefer `git mv plans/<plan>.md plans/closed/`
over deletion so history is preserved.
