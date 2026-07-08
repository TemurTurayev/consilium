---
description: Get a second (and third) opinion — run an anonymized multi-model council on a question and synthesize a recommendation
argument-hint: [question or decision to deliberate]
---

Run a Consilium council on the following question and report the synthesized recommendation:

$ARGUMENTS

Use the `council_run` MCP tool from the consilium server. If the consilium tools are not available, run `which consilium`; if the binary is missing, tell the user to install it (https://github.com/TemurTurayev/consilium#install) and stop.

Otherwise, pass the question to `council_run`, then present, in order: the chairman's synthesis, each member's key points, and any point of disagreement worth the user's attention.
