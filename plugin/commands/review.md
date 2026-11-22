---
description: Cross-model review of a diff — have a different model family audit the current changes
argument-hint: [optional path, or "staged"]
---

Review the current code changes with a fresh, independent model via Consilium.

1. Get the diff: if `$ARGUMENTS` names a path, run `git diff -- $ARGUMENTS`; if it is `staged`, run `git diff --staged`; otherwise run `git diff HEAD`.
2. Pass the diff to the `review_diff` MCP tool, always with `author_model`: your own model for a diff you wrote, or the worker's `model_used` if a worker wrote it. It puts a reviewer of a different model family than the author first, to counter self-preference bias. Without `author_model` the diff is reviewed as if it were yours, and no refutation lifts a block — nobody can be shown independent of an author that was not named.
3. A second model of another family rules on each critical and important finding. Report findings by severity with their `ruling` and `ruling_reason`: act on the confirmed ones, give the reason before dropping a refuted one, and decide the unclear ones yourself. Say who reviewed (`model_used`) and who ruled (`examined_by`, `examiner_independence`). If `cross_examination` starts with `unavailable`, say the findings were not checked by a second model.
4. Treat an unparseable review as a failure, and `blocking: true` as a blocker: a critical finding that still stands. Only a refutation by a model independent of both the author and the reviewer (`examiner_independence: independent`) lifts a block; a ruling from either side is information, not a verdict. If `cross_family` is `degraded_same_family`, say that no reviewer of another family was available; if it is `unknown_author`, say the author could not be identified and the review was ordered as for your own diff.

If the consilium tools are not available, run `which consilium`; if it is missing, point the user to https://github.com/TemurTurayev/consilium#install and stop.
