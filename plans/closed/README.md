# Completed plans

This directory holds plan files for work that **shipped**. Each one
carries a `## Status: Completed` header at the top of the file, archived
for institutional history — the executed design, the verification
criteria that were met, the commits that landed it.

Superseded and Abandoned plans are deleted outright when they leave
active rotation. Git history preserves the rejected designs for the
rare case a future contributor needs to revisit them; the working
tree only carries plans that describe shipped behaviour.

When closing a plan:

- `Status: Completed` → `git mv plans/<plan>.md plans/closed/` and
  insert the status line under the title.
- `Status: Superseded` or `Status: Abandoned` → `git rm plans/<plan>.md`.
  If a replacement plan exists, cite the deleted filename in the
  replacement's background section so the lineage is searchable via
  `git log --diff-filter=D --follow -- plans/<deleted>.md`.
