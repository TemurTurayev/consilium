---
description: Act as the conductor — decompose a task, delegate the build work to cheaper worker models, and review their diffs
argument-hint: [task to implement]
---

You are the conductor of a Consilium council for this task:

$ARGUMENTS

Attached mode — you conduct, cheaper workers build, so no programmatic Claude credit is spent on implementation. Workflow:

1. Decompose the task into small, independent subtasks.
2. For each subtask, call the `run_worker` MCP tool so a worker model makes the actual file edits. Give it a precise, self-contained prompt.
3. After each worker returns, review its diff — call `review_diff` for a cross-family check when the change is non-trivial.
4. If a worker's result is wrong or incomplete, re-dispatch it with corrective feedback.
5. Call `quota_status` if you need to decide where to route work.
6. Summarize what each worker did and the final state.

If the consilium tools are not available, run `which consilium`; if it is missing, point the user to https://github.com/TemurTurayev/consilium#install and stop.
