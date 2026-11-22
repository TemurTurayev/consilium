---
description: Cross-model review of a diff — have a different model family audit the current changes
argument-hint: [optional path, or "staged"]
---

Review the current code changes with a fresh, independent model via Consilium.

1. Get the diff: if `$ARGUMENTS` names a path, run `git diff -- $ARGUMENTS`; if it is `staged`, run `git diff --staged`; otherwise run `git diff HEAD`.
2. Pass the diff to the `review_diff` MCP tool. It puts a reviewer of a different model family than the author first, to counter self-preference bias: with no `author_model` the diff is taken to be yours (Claude's); if a worker wrote it, pass that worker's `model_used` as `author_model`.
3. Report findings by severity, and who reviewed (`model_used`). Treat an unparseable review as a failure, and any critical finding as a blocker. If `cross_family` is `degraded_same_family`, say that no reviewer of another family was available; if it is `unknown_author`, say the author could not be identified and the review was ordered as for your own diff.

If the consilium tools are not available, run `which consilium`; if it is missing, point the user to https://github.com/TemurTurayev/consilium#install and stop.
