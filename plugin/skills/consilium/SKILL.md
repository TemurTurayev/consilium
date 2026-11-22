---
name: consilium
description: Use when the user wants a second opinion from other AI models, wants to offload implementation work to cheaper model subscriptions (Codex, Gemini, Grok) while Claude conducts, wants a cross-model review of a diff, or mentions Consilium or a "council". Requires the consilium CLI installed and its MCP server connected.
---

# Consilium — you are the conductor

Consilium turns this live session into the conductor of a multi-model council. You plan and review; cheaper worker models (Codex, Gemini, Grok) do the bulk implementation on the user's own subscriptions. Because the conductor is this interactive session, no programmatic Claude credit is spent on the build work.

## Tools (from the `consilium` MCP server)

- `lineup` — who is on the council: the worker labels `run_worker` accepts (each with its models), the chairman, the reviewer, and which configuration file they came from. Call it first.
- `run_worker` — dispatch a self-contained subtask to a worker model, which makes real file edits. Returns the worker's own diff (what changed while it ran — not edits that were already uncommitted), the build/test result, and the model that did the work. Workers in one repository run one at a time.
- `council_run` — anonymized multi-model deliberation on a question, then a chairman synthesis. Use for hard design decisions or second opinions. `fallbacks` lists any member that did not answer on its configured model.
- `review_diff` — have a *different* model family audit a diff (counters self-preference bias). For a worker's diff pass `author_model` — the `model_used` that `run_worker` returned — so the reviewer is picked against the real author; without it the diff is taken to be yours. `cross_family` says whether a different family was available.
- `quota_status` — per-provider token usage over the rolling window. Use to decide where to route work.
- `search_recall` / `page_in` — search past runs (councils, reviews and worker runs included) and page one back in by id.

## When to reach for this

- The user asks for a second opinion, or wants another model to weigh in or cross-check.
- There is a batch of well-specified build work worth offloading to cheaper workers while you conduct.
- The user wants a diff reviewed by a fresh, independent model.
- The user explicitly mentions Consilium or a council.

## How to conduct

1. Call `lineup` to learn the worker labels.
2. Decompose the task into small, independent subtasks.
3. Call `run_worker` for each one with a precise, self-contained prompt. Keep subtasks disjoint so their edits do not collide.
4. Call `review_diff` on non-trivial results, passing the worker's `model_used` as `author_model`; re-dispatch with corrective feedback if a result is wrong or incomplete.
5. Synthesize and report what each worker did and the final state.

## When something fails

- A tool's `error` carries the provider CLI's own message. "not signed in" means that CLI needs a login: run `consilium auth` in a terminal and follow what it says (for Claude, the token belongs in `~/.consilium/secrets.env`).
- "model unavailable" means a configured model was retired or is not in the plan: `consilium models --write` switches to current ones.
- If the consilium tools are unavailable altogether, the CLI is likely not installed — run `which consilium`; if it is missing, point the user to the install instructions at https://github.com/TemurTurayev/consilium#install and stop.
