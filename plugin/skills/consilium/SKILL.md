---
name: consilium
description: Use when the user wants a second opinion from other AI models, wants to offload implementation work to cheaper model subscriptions (Codex, Gemini, Grok) while Claude conducts, wants a cross-model review of a diff, or mentions Consilium or a "council". Requires the consilium CLI installed and its MCP server connected.
---

# Consilium — you are the conductor

Consilium turns this live session into the conductor of a multi-model council. You plan and review; cheaper worker models (Codex, Gemini, Grok) do the bulk implementation on the user's own subscriptions. Because the conductor is this interactive session, no programmatic Claude credit is spent on the build work.

## Tools (from the `consilium` MCP server)

- `run_worker` — dispatch a self-contained subtask to a worker model, which makes real file edits and returns its diff. Use for mechanical or well-specified implementation work.
- `council_run` — anonymized multi-model deliberation on a question, then a chairman synthesis. Use for hard design decisions or second opinions.
- `review_diff` — have a *different* model family audit a diff (counters self-preference bias). Use before trusting a non-trivial change.
- `quota_status` — per-provider token usage over the rolling window. Use to decide where to route work.
- `search_recall` / `page_in` — search past run transcripts and page a full one back in.

## When to reach for this

- The user asks for a second opinion, or wants another model to weigh in or cross-check.
- There is a batch of well-specified build work worth offloading to cheaper workers while you conduct.
- The user wants a diff reviewed by a fresh, independent model.
- The user explicitly mentions Consilium or a council.

## How to conduct

1. Decompose the task into small, independent subtasks.
2. Call `run_worker` for each one with a precise, self-contained prompt. Keep subtasks disjoint so their edits do not collide.
3. Call `review_diff` on non-trivial results; re-dispatch with corrective feedback if a result is wrong or incomplete.
4. Synthesize and report what each worker did and the final state.

If the consilium tools are unavailable, the CLI is likely not installed — run `which consilium`; if it is missing, point the user to the install instructions at https://github.com/TemurTurayev/consilium#install and stop.
