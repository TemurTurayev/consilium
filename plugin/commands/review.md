---
description: Cross-model review of a diff — have a different model family audit the current changes
argument-hint: [optional path, or "staged"]
---

Review the current code changes with a fresh, independent model via Consilium.

1. Get the diff: if `$ARGUMENTS` names a path, run `git diff -- $ARGUMENTS`; if it is `staged`, run `git diff --staged`; otherwise run `git diff HEAD`.
2. Pass the diff to the `review_diff` MCP tool — it routes to a reviewer of a different model family than usually wrote the code, to counter self-preference bias.
3. Report findings by severity. Treat an unparseable review as a failure, and any critical finding as a blocker.

If the consilium tools are not available, run `which consilium`; if it is missing, point the user to https://github.com/TemurTurayev/consilium#install and stop.
