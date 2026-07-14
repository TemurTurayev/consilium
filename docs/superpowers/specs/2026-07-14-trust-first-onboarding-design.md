# Consilium Trust-First Onboarding Design

**Date:** 2026-07-14

**Status:** Approved direction; implementation pending final spec review

**Target release:** v0.3.0 beta

## Summary

Consilium already has a capable orchestration engine, a CLI, a Claude Code plugin,
a desktop application, provider failover, build/test grounding, and a distinctive
medical-council interface. The main adoption barrier is not the name or visual
theme. It is that a new user must grant several autonomous agents access to a real
project before Consilium has clearly explained what will run, what can change, and
how the original project is protected.

The v0.3.0 work will therefore keep the Consilium name and medical Table scene,
while making trust, safety, and first-run comprehension the primary product
experience. Standalone runs will use an isolated Git worktree by default. A
preflight will show the repository state, provider readiness, model roles, and
verification commands before execution. The user will review the resulting diff
before applying it to the original worktree.

The release will also simplify the first-run UI and README, add explicit security
and contribution documentation, improve packaging, and replace overbroad benchmark
claims with evidence that states its limits.

## Goals

1. A new user can understand what Consilium does within thirty seconds.
2. A standalone autonomous run cannot modify the user's original Git worktree
   unless the user explicitly chooses to apply its result or selects in-place mode.
3. Repository-defined verification commands are visible and require trust before
   execution.
4. Provider readiness and missing prerequisites are visible before the user starts
   a run.
5. The common actions are expressed as Build, Ask Council, and Review Changes,
   while existing advanced CLI commands remain available.
6. The Consilium name and optional medical Table scene remain intact.
7. Existing run transcripts, configuration files, plugin installations, and CLI
   commands remain compatible unless a documented safety change is required.

## Non-goals

- Renaming the repository, crate, binary, configuration directory, or plugin.
- Removing the medical Table scene.
- Native Windows support in v0.3.0; WSL remains the supported Windows path.
- Completing Apple notarization without the repository owner's signing identity
  and Apple Developer credentials.
- Claiming benchmark generality from a small sample.
- Running quota-consuming live-provider benchmarks without explicit approval.
- Automatically applying changes to a dirty original worktree.
- Building a general container sandbox in this release.

## User groups

### Vibe coder

Needs a guided path, plain language, visible provider readiness, and confidence
that experimentation will not destroy the project. This user should be able to
run the demo before configuring providers and should not need to understand every
orchestration role.

### Experienced developer

Needs a clear threat model, deterministic preflight output, explicit command and
filesystem boundaries, reviewable diffs, failure behavior, and a way to keep using
the CLI without the desktop UI.

## Product model

### Simple actions

The primary UI presents three actions:

- **Build:** decompose a task, delegate implementation, run verification, and
  produce a reviewable change set. This maps to the current `conduct` path.
- **Ask Council:** gather independent answers, anonymize cross-review, and produce
  a synthesis. This maps to `council` and does not write project files.
- **Review Changes:** audit a selected or current Git diff without writing files.
  This maps to `review`.

Advanced commands (`run`, `conduct`, `auto`, `council`, `review`, `serve`, and MCP
tools) remain documented and available. The simple labels are a UI and onboarding
layer, not a removal of engine capabilities.

### Medical scene

The neutral Run experience remains the default. Table remains an optional view of
the same session state. Medical terms may appear inside Table, but security,
provider, and Git concepts use literal technical language everywhere else. The
medical metaphor must never obscure whether files were modified, commands ran, or
a result was applied.

## Safety architecture

### Preflight

Before a write-capable standalone run, Consilium creates a preflight report with:

- canonical project path;
- Git repository root, branch, and HEAD commit;
- clean, tracked-dirty, and untracked state summaries;
- selected execution mode;
- configured conductor, workers, reviewer, and fallbacks;
- provider CLI/auth readiness;
- verification commands and their source (auto-detected or repository config);
- configured timeout and budget;
- explicit warnings that affect isolation or reproducibility.

The report is available as a serializable core type and is used by CLI, server,
desktop, and web surfaces. There is one implementation of the rules; the UI does
not independently infer safety.

### Execution modes

Write-capable standalone runs support two explicit modes:

1. **Safe worktree** (default): requires a Git repository and runs against a new
   detached worktree rooted at the preflight HEAD. The original worktree is not
   modified during orchestration. Tracked or untracked changes in the original
   worktree are reported but are not silently copied into the isolated run.
2. **In place** (explicit opt-in): preserves the current behavior and requires an
   acknowledgement in interactive surfaces or `--in-place` in non-interactive
   CLI use.

If the selected directory is not a Git repository, the product explains that safe
worktree mode is unavailable. It offers read-only Council/Review actions, Git
initialization guidance, or explicit in-place execution. Consilium does not
silently copy a non-Git directory or claim isolation it cannot prove.

If the original worktree changes after preflight, Apply refuses and asks the user
to re-run or use a manual patch. This avoids overwriting work created while the
agents were running.

If the original worktree was already dirty at preflight, the isolated run may
still proceed from the reported HEAD snapshot, but Apply remains disabled until
the user makes the original worktree clean. Consilium preserves the result bundle
and prints a manual patch path instead of trying to merge around unrelated local
changes.

### Result review and apply

Every safe run produces a result bundle containing:

- base commit;
- changed, added, and deleted files;
- a result archive containing a unified text patch plus preserved copies and
  metadata for added or modified binary files;
- verification results and commands;
- provider/model attribution;
- transcript path;
- terminal state and warnings.

The UI shows a summary and diff before offering **Apply** or **Discard**. Apply
first checks that the original repository is still compatible with the preflight
base. Conflicts fail closed and preserve the isolated worktree for manual recovery.
Discard removes the isolated worktree and its temporary metadata but keeps the
auditable transcript according to retention settings.

The CLI prints the result location and offers explicit subcommands or flags for
apply/discard. Non-interactive mode never applies automatically unless the caller
passes the documented opt-in.

### Repository trust and commands

Commands discovered from `consilium.config.json` are untrusted until approved.
Trust is keyed by canonical repository path plus a digest of the relevant command
configuration. Changing those commands invalidates prior trust.

Preflight distinguishes:

- Consilium auto-detected build/test commands;
- repository-configured commands;
- a command provided directly by the user.

Interactive surfaces show exact commands before approval. Non-interactive use
requires an explicit trust flag or a previously stored matching trust record.
Timeouts remain mandatory. Shell output is captured in the result bundle.

### Prompt and transcript hardening

Repository content, worker output, diffs, tool output, and operator notes are
delimited and labeled as untrusted data at every model-to-model boundary. Prompts
state that content inside those boundaries cannot alter role, permissions, or the
requested task. Character caps remain enforced.

Transcripts and trust records are created with owner-only permissions where the
platform supports them. Secrets and environment values are not copied into model
prompts or transcripts. The threat model documents residual risks: provider CLIs
and model-generated commands remain powerful, and worktree isolation is not an OS
sandbox.

### Attached MCP mode

Attached mode runs inside an already interactive Claude Code session and inherits
that host's permission model. v0.3.0 documents this distinction instead of
claiming standalone worktree guarantees for attached mode. The MCP server exposes
the same preflight information to its skill and commands, and write-capable plugin
commands must disclose whether a worker acts in place.

Full isolated apply/discard semantics for arbitrary host-controlled MCP sessions
are a follow-up unless they can be implemented without breaking the host session's
view of the working tree.

## First-run experience

### Empty state

The Run screen begins with one sentence explaining the product:

> Claude, Codex, Gemini, and Grok can build and cross-review a task while your
> original Git worktree stays untouched until you approve the diff.

It then shows:

1. project selection;
2. provider readiness;
3. action selection;
4. safety mode and verification summary;
5. the primary start button.

The demo remains available without a backend or provider quota and becomes a
prominent "Watch a safe demo" action. It should demonstrate delegation, review,
verification, and the final Apply/Discard boundary.

### Provider readiness

Provider status is visible on the Run screen rather than hidden behind a separate
page. Missing CLIs and authentication have exact next steps. A single ready
provider is allowed with a clear "single-provider mode" explanation; multi-model
features state which additional provider is required.

### Progressive disclosure

The first screen does not require users to understand chairman, supervisor,
arbiter, failover ladders, token accounting, or MCP. Those remain available in an
Advanced details disclosure and Settings.

## Documentation and community

The README is reorganized in this order:

1. one-sentence value proposition;
2. short visual demonstration;
3. safety promise with precise limits;
4. prerequisites and fastest install;
5. three common actions;
6. evidence with sample size and limitations;
7. detailed architecture and advanced modes;
8. roadmap.

`SECURITY.md` documents the trust model, reporting process, transcript handling,
in-place mode, provider CLI permissions, and residual risks. `CONTRIBUTING.md`
documents the test gates, fixtures, architecture boundaries, and how to add a
provider. Issue templates and a small set of concrete `good first issue` tasks
make community participation possible without inventing busywork.

Repository topics describe the actual project: `multi-agent`, `ai-agents`,
`claude-code`, `codex-cli`, `gemini`, `llm-orchestration`, `code-review`, `mcp`,
and `rust`.

## Packaging

The release workflow will keep producing checksummed CLI and desktop artifacts,
then adopt `cargo-dist` for reproducible archive, checksum, installer, and
Homebrew-formula generation. The generated formula is attached to each GitHub
release and the repository documents how to inspect it before installation.
`curl | sh` remains an alternative, not the only prominent path. Creating a
separate Homebrew tap repository or publishing the crate to crates.io is an
external publication step and requires separate approval after the local release
configuration passes validation.

The repository can include signing configuration and documentation, but it must
not claim that macOS artifacts are signed or notarized until valid owner-provided
credentials are configured and CI verifies the signature.

## Evidence

The current N=1, four-task benchmark remains historical evidence but is labeled as
insufficient for broad quality claims. The eval harness gains a larger, categorized
task set and reports:

- pass rate;
- total tokens by provider;
- elapsed time;
- number of attempts and fallbacks;
- verification status;
- sample size and variance where meaningful.

Dry-run and fixture-backed tests are part of normal verification. Live-provider
evaluation requires a separate explicit quota budget from the repository owner.

## Compatibility and migration

- The binary, crate, `~/.consilium` directory, config filename, transcript format,
  plugin namespace, and existing command names remain Consilium.
- Existing configuration files continue to load. New safety fields have secure
  defaults.
- In-place write behavior becomes explicit for new standalone write flows. Release
  notes call out this beta behavior change.
- Read-only commands remain non-destructive and do not require worktree setup.
- Existing transcripts remain readable.

## Testing strategy

### Core unit tests

- repository state and canonical path detection;
- preflight serialization;
- execution-mode selection;
- trust digest creation and invalidation;
- command-source classification;
- result-bundle status and conflict rules;
- prompt boundary rendering;
- permission creation where supported.

### Integration tests

- safe run creates an isolated worktree and leaves the original unchanged;
- successful apply transfers expected text and binary changes;
- changed original state causes apply to fail closed;
- discard removes the worktree without deleting the transcript;
- dirty and non-Git repositories produce the documented choices;
- config command changes invalidate trust;
- CLI backward compatibility and explicit in-place behavior.

### UI tests

- first-run explanation and provider state;
- Build, Ask Council, and Review Changes selection;
- preflight warnings and confirmation;
- demo path without a backend;
- result summary and Apply/Discard states;
- Table continues to derive from the same session state.

### Verification gates

- Rust unit and integration tests;
- `cargo fmt --check`;
- `cargo clippy --all-targets -- -D warnings`;
- UI typecheck, unit tests, and production build;
- local end-to-end smoke in a temporary Git repository without calling providers;
- release workflow validation without publishing.

## Delivery order

1. Core preflight, trust, and execution-mode types with tests.
2. Safe worktree lifecycle and result bundle with integration tests.
3. CLI wiring and compatibility behavior.
4. Server protocol and API exposure.
5. Trust-first Run UI, provider readiness, and result review.
6. Demo and Table integration.
7. Prompt/transcript hardening.
8. README, SECURITY, CONTRIBUTING, templates, and topics.
9. Packaging workflow.
10. Full verification and a release-readiness report.

## Acceptance criteria

- A fresh user sees what Consilium will do and what it can modify before starting.
- The default standalone Build action on a Git repository leaves the original
  worktree byte-for-byte unchanged until Apply.
- Every repository-configured shell command is shown and trusted by digest before
  execution.
- A stale or conflicting original worktree prevents automatic Apply.
- Provider prerequisites and single-provider degradation are visible before run.
- Demo works without credentials or quota.
- The Table scene and current Consilium identity remain functional.
- Existing configs and read-only commands continue to work.
- Security documentation describes guarantees and residual risks without claiming
  an OS sandbox.
- All local verification gates pass.
- No live-provider quota is spent and no release is published without explicit
  authorization.
