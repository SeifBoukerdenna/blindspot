# Task notes and resuming work

For substantial multi-step work, create a short descriptive .md file here using
[TEMPLATE.md](TEMPLATE.md). Small, obvious fixes need no ceremony. Keep the note in Git with
the work, but the user handles staging/committing. Never edit another active task's note
without coordinating. One task per file avoids a shared constantly-changing status page.

## Resume

1. Read root AGENTS.md and run `make agent-context`.
2. Open the task named by the user. If none is named, identify active notes; do not silently
   take over an unrelated task. Compare its base revision and scope with the current diff.
3. Read the relevant source-map section, target code and tests. Recheck claims that could
   have changed; a historical pass or checkbox is not current verification.
4. Continue the first unfinished acceptance criterion. Avoid repeating completed exploration
   unless the source or requirements changed.

## Keep it useful

- Use `Status: active`, `Status: blocked` or `Status: complete` and an ISO `Updated:` date.
- Record requirements, decisions/alternatives, owned files, pre-existing changes and the next
  action. Do not copy a conversation, raw tool logs, local document paths, prompts or secrets.
- Update at meaningful milestones, before a handoff, and when stopping with unfinished work.
- Record exact commands, outcomes, limitations and the revision/working-tree state tested.
  Machine reports live in ignored build/agent/ and may disappear; summarize essential evidence
  here. Missing reports do not make old test claims current.
- Mark complete only when acceptance criteria are met. Distinguish implementation, validation,
  local installation and GitHub publication. Do not silently drop requirements.
- When a decision materially changes architecture or safety, explain why and link the relevant
  source/design document. Do not manufacture a new architecture-decision file for every edit.

Example prompt: “Read AGENTS.md, then resume docs/tasks/search-filter-fix.md. Reconcile the note
with the working tree, continue the next step, and update the handoff with actual verification.”
