# Community, Release, and Evidence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Consilium understandable and trustworthy from its repository page, provide honest reproducible evaluation evidence, and prepare a checksum-verified `v0.3.0-beta` release pipeline without publishing anything automatically.

**Architecture:** Documentation leads with the user problem, 30-second safe flow, limitations, and prerequisites before internal architecture. The eval harness emits a versioned schema with explicit verification state and reproducibility metadata, while a larger zero-quota fixture suite validates the harness itself. Release preparation uses a pinned cargo-dist plan for CLI artifacts, a coordinated desktop path, strict version/changelog checks, and a fail-closed installer; publication, signing claims, crates.io, and external Homebrew tap creation remain separate authorized operations.

**Tech Stack:** Markdown, GitHub issue forms/Actions, Rust/Serde eval harness, shell validation scripts, cargo-dist 0.32.0, existing Tauri release build, Node 22 UI build.

## Global Constraints

- Target release is `v0.3.0-beta`; keep **Consilium** and its medical Table identity.
- README order is: value proposition → 30-second flow → safety model/limits → prerequisites/install → Build/Ask Council/Review Changes → demo → attached MCP disclosure → architecture/eval links → contribution/security.
- Do not lead with an architecture matrix or broad benchmark claim.
- Historical evidence must be labelled exactly as N=1 over four small Rust tasks and must not imply general superiority.
- Eval dry-run remains the default and prints `no quota spent`; live provider execution still requires the existing explicit `--spend-quota` gate.
- Never run live eval/provider probes while implementing or testing this plan.
- Eval schema v2 records category, language, difficulty, `passed|failed|not_run`, all four providers, attempts/fallbacks, sample size, and variance only when N ≥ 2.
- Raw local eval results remain ignored; publish only audited aggregate documentation with date, suite, N, and limitations.
- Installer checksum retrieval and verification are fail closed.
- cargo-dist is pinned to `0.32.0`, packages only the `consilium` CLI, targets `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu`, and preserves `.tar.gz` asset compatibility.
- Do not configure crates.io publishing, a Homebrew tap token, release signing credentials, macOS notarization claims, or a desktop Cask.
- Do not push a tag, create a GitHub Release, or publish packages as part of this plan.
- Repository topics and the three specified `good first issue` tickets are the only remote metadata mutations in scope; verify the exact repository before applying them.
- Preserve the user's untracked `.claude/` directory and never stage it.

---

## File Structure

- `README.md`: concise trust-first product entrance.
- `docs/architecture.md`: detailed engine, protocol, and permission-model reference moved out of README.
- `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `.github/ISSUE_TEMPLATE/*`, `.github/PULL_REQUEST_TEMPLATE.md`: community trust surface.
- `core/src/orchestrator/eval.rs`: schema-v2 metrics and honest aggregation.
- `eval/README.md`: reproducible fixture/live-eval instructions and quota boundary.
- `eval/tasks/*`: 12 self-contained fixture repositories with protected tests and reference patches.
- `docs/eval/historical-v0.2.0-n1.md`: audited aggregate-only historical evidence.
- `script/validate_release.sh`: versions, tag, changelog, and asset-contract gate.
- `dist-workspace.toml`: pinned cargo-dist CLI plan.
- `.github/workflows/ci.yml` and `.github/workflows/release.yml`: full CI and coordinated release preparation.
- `install.sh`: checksum-required compatibility installer.

### Task 1: Trust-first README and community files

**Files:**
- Modify: `README.md`
- Create: `docs/architecture.md`
- Create: `docs/assets/safe-demo.png`
- Create: `SECURITY.md`
- Create: `CONTRIBUTING.md`
- Create: `CHANGELOG.md`
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`
- Create: `.github/ISSUE_TEMPLATE/feature_request.yml`
- Create: `.github/ISSUE_TEMPLATE/config.yml`
- Create: `.github/PULL_REQUEST_TEMPLATE.md`

**Interfaces:**
- Consumes: approved trust-first specification and actual v0.3 UI/CLI behavior from the first two plans.
- Produces: repository landing page, contributor path, security-reporting path, and structured issue intake.

- [ ] **Step 1: Write a failing documentation contract test**

Create `core/tests/docs_contract_test.rs`:

```rust
#[test]
fn readme_leads_with_safe_user_flow_before_architecture() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md")).unwrap();
    let flow = readme.find("## The 30-second flow").unwrap();
    let safety = readme.find("## What safe worktree mode protects").unwrap();
    let architecture = readme.find("## Architecture").unwrap();
    assert!(flow < safety && safety < architecture);
    assert!(readme.contains("Build"));
    assert!(readme.contains("Ask Council"));
    assert!(readme.contains("Review Changes"));
    assert!(readme.contains("Attached MCP edits the host workspace directly"));
    assert!(readme.contains("docs/assets/safe-demo.png"));
    assert!(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("docs/assets/safe-demo.png").is_file());
    assert!(!readme.contains("Consilium beats"));
}

#[test]
fn repository_has_the_expected_community_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for path in [
        "SECURITY.md", "CONTRIBUTING.md", "CHANGELOG.md",
        ".github/ISSUE_TEMPLATE/bug_report.yml",
        ".github/ISSUE_TEMPLATE/feature_request.yml",
        ".github/PULL_REQUEST_TEMPLATE.md",
        "docs/architecture.md",
    ] {
        assert!(root.join(path).is_file(), "missing {path}");
    }
}
```

- [ ] **Step 2: Run and confirm the order/files fail**

Run: `cargo test -p consilium --test docs_contract_test`

Expected: FAIL because headings/order/community files are absent.

- [ ] **Step 3: Rewrite README with the exact opening and section order**

Use this opening verbatim:

```markdown
# Consilium

Consilium lets several coding agents plan, implement, verify, and review one change while you stay in control of what reaches your checkout.

By default, write-capable standalone runs work in an isolated Git worktree. Your current checkout is not edited until you inspect the diff and choose **Apply**. You can also **Discard** the worktree while keeping the audit record.

## The 30-second flow

1. Choose a project and one action: **Build**, **Ask Council**, or **Review Changes**.
2. Read the safety preview: path, Git state, models, exact verification commands, timeout, and budget.
3. Start the run. Build uses a detached worktree by default; the other two actions are read-only.
4. Inspect changed files and verification results, then choose **Apply** or **Discard**.

> Try the built-in demo first. It needs no provider account, backend, or quota.
```

Follow with `What safe worktree mode protects`, `What it does not protect`, `Prerequisites`, `Install`, `First run`, `The three actions`, `Demo`, `Attached MCP`, `Architecture`, `Evaluation evidence`, `Contributing`, `Security`, and `License`. Move detailed role/status tables and protocol explanation to `docs/architecture.md` and link them once.

- [ ] **Step 4: Capture the real safe-demo result screen**

Start the already-implemented UI with `npm --prefix ui run dev -- --host 127.0.0.1 --port 4173`, open `http://127.0.0.1:4173`, choose `Watch a safe demo`, advance to Result Review, set the viewport to 1440×900, and capture the application surface as `docs/assets/safe-demo.png`. The image must visibly include `Original checkout unchanged`, changed files, verification summary, Apply, and Discard; it must contain no local username, real filesystem path, token, or provider account. Add immediately after the opening paragraph:

```html
<p align="center">
  <img src="docs/assets/safe-demo.png" alt="Consilium safe demo showing an unchanged original checkout, reviewed diff, verification results, Apply, and Discard actions" width="960">
</p>
```

- [ ] **Step 5: Add precise limitation and attached-mode copy**

Use these exact sentences:

```text
Safe worktree mode protects your current Git checkout from agent edits until Apply. It is not an operating-system sandbox: provider CLIs still inherit the Consilium process environment and may access resources allowed by their own permission model.

Attached MCP edits the host workspace directly and inherits the host's permissions. Consilium displays the path and verification commands, but v0.3 does not redirect an attached host session into the standalone Apply/Discard worktree flow.

For a non-Git folder, Consilium cannot create a safe worktree. Use a read-only action, initialize Git, or explicitly choose in-place execution.
```

- [ ] **Step 6: Add community templates with actionable fields**

The bug form requires Consilium version, surface (`Desktop`, `CLI`, `Attached MCP`, `Other`), OS, action, execution mode, Git state, expected behavior, actual behavior, and a redacted transcript excerpt. The feature form requires problem, proposed outcome, affected surface, and safety implications. Disable blank issues in `config.yml` without inventing a private security email.

`SECURITY.md` says: do not open public issues for undisclosed vulnerabilities; use GitHub Private Vulnerability Reporting when enabled; if the repository UI does not show that option, open a non-sensitive discussion asking for a private contact without sharing exploit details. Do not claim a response-time SLA.

It also documents safe-worktree guarantees, explicit in-place risk, attached MCP host permissions, owner-only transcript/trust/result files on Unix, transcript redaction expectations, retention/deletion locations under `~/.consilium`, inherited provider CLI/environment access, and the fact that worktree isolation is not an OS sandbox. `CONTRIBUTING.md` lists the exact Rust/UI/release gates, module boundaries from the three plans, fixture rules, and the adapter files/tests required when adding a provider.

- [ ] **Step 7: Run docs tests and link checks**

Run: `cargo test -p consilium --test docs_contract_test && rg -n "FIXME|XXX|Consilium beats" README.md SECURITY.md CONTRIBUTING.md docs .github/ISSUE_TEMPLATE`

Expected: tests PASS and the search returns no unfinished-marker or broad-superiority text.

- [ ] **Step 8: Commit**

```bash
git add README.md docs/architecture.md docs/assets/safe-demo.png SECURITY.md CONTRIBUTING.md CHANGELOG.md .github/ISSUE_TEMPLATE .github/PULL_REQUEST_TEMPLATE.md core/tests/docs_contract_test.rs
git commit -m "docs: lead with the safe Consilium workflow"
```

### Task 2: Eval report schema v2 and honest statistics

**Files:**
- Modify: `core/src/orchestrator/eval.rs`
- Modify: `core/src/main.rs`
- Modify: `core/tests/eval_test.rs`
- Create: `eval/README.md`

**Interfaces:**
- Consumes: existing `Approach`, `run_verify`, conduct attempt/fallback metadata, and provider quota snapshots.
- Produces: `EVAL_SCHEMA_VERSION`, `VerificationStatus`, expanded `EvalTask`, `ProviderTokens`, `TrialResult`, `MetricSummary`, `SuiteReport`, and schema-v2 JSON/Markdown.

- [ ] **Step 1: Write failing schema/statistics tests**

```rust
#[test]
fn report_v2_records_verification_provenance_and_all_providers() {
    let report = fixture_report(vec![trial(
        "parse-kv", "parsing", "rust", "small", VerificationStatus::Passed,
        ProviderTokens { claude: 1, codex: 2, gemini: 3, grok: 4 }, 2, 1,
    )]);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["results"][0]["verification"], "passed");
    assert_eq!(json["results"][0]["tokens"]["grok"], 4);
    assert_eq!(json["results"][0]["attempts"], 2);
    assert_eq!(json["results"][0]["fallbacks"], 1);
}

#[test]
fn variance_is_absent_for_n1_and_present_for_n2() {
    let one = summarize(&[100]);
    assert_eq!(one.n, 1);
    assert_eq!(one.std_dev, None);
    let two = summarize(&[100, 300]);
    assert_eq!(two.n, 2);
    assert_eq!(two.mean, 200.0);
    assert!(two.std_dev.unwrap() > 0.0);
}
```

- [ ] **Step 2: Run and confirm schema v1 cannot satisfy tests**

Run: `cargo test -p consilium --test eval_test report_v2 && cargo test -p consilium orchestrator::eval::tests::variance`

Expected: FAIL with missing fields/types.

- [ ] **Step 3: Define the exact v2 schema**

```rust
pub const EVAL_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus { Passed, Failed, NotRun }

#[derive(Debug, Clone, Deserialize)]
pub struct EvalTask {
    pub name: String,
    pub category: String,
    pub language: String,
    pub difficulty: String,
    pub prompt: String,
    #[serde(default)] pub context: String,
    #[serde(default)] pub verify: Option<VerifyConfig>,
    #[serde(default)] pub protected_paths: Vec<String>,
    #[serde(skip)] pub repo_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ProviderTokens { pub claude: u64, pub codex: u64, pub gemini: u64, pub grok: u64 }

#[derive(Debug, Clone, Serialize)]
pub struct TrialResult {
    pub task: String,
    pub category: String,
    pub language: String,
    pub difficulty: String,
    pub approach: String,
    pub trial: u32,
    pub verification: VerificationStatus,
    pub pipeline_ok: bool,
    pub attempts: u32,
    pub fallbacks: u32,
    pub tokens: ProviderTokens,
    pub wall_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")] pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricSummary { pub n: usize, pub mean: f64, pub median: f64, pub std_dev: Option<f64> }

#[derive(Debug, Clone, Serialize)]
pub struct SuiteReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub suite_commit: String,
    pub trials: u32,
    pub results: Vec<TrialResult>,
    pub aggregate: Aggregate,
}
```

Use sample standard deviation (`n - 1` denominator) only for N ≥ 2. Keep a compatibility reader for old ignored local results if any comparison command reads them; new writes are v2 only.

- [ ] **Step 4: Capture attempts/fallbacks without changing score semantics**

Independent external verification remains the only score: `Passed` only if commands ran and all passed, `Failed` if any command ran and failed, `NotRun` when no verifier command resolved. Pull conduct attempts/fallbacks from its outcome transcript metadata and solo fallbacks from `run_with_failover`; pipeline self-report remains informational.

- [ ] **Step 5: Preserve and test the quota gate**

Run: `cargo run -q -p consilium -- eval`

Expected: exit success, list tasks/approaches/call estimates, print the exact phrase `no quota spent`, create no result file, and open no provider configuration.

- [ ] **Step 6: Run all eval tests**

Run: `cargo test -p consilium --test eval_test && cargo test -p consilium orchestrator::eval`

Expected: PASS for v2 serialization, Grok totals, verification states, category grouping, N, and variance.

- [ ] **Step 7: Commit**

```bash
git add core/src/orchestrator/eval.rs core/src/main.rs core/tests/eval_test.rs eval/README.md
git commit -m "feat: make eval evidence explicit and versioned"
```

### Task 3: Twelve-task zero-quota fixture suite with reference solutions

**Files:**
- Modify: `eval/tasks/add-greeting/task.json`
- Modify: `eval/tasks/fix-sum/task.json`
- Modify: `eval/tasks/parse-duration/task.json`
- Modify: `eval/tasks/parse-kv/task.json`
- Create: `eval/tasks/*/solution.patch` for all 12 tasks
- Create: `eval/tasks/normalize-email/task.json` and `repo/`
- Create: `eval/tasks/clamp-retry/task.json` and `repo/`
- Create: `eval/tasks/json-content-type/task.json` and `repo/`
- Create: `eval/tasks/filter-active/task.json` and `repo/`
- Create: `eval/tasks/path-extension/task.json` and `repo/`
- Create: `eval/tasks/dedupe-stable/task.json` and `repo/`
- Create: `eval/tasks/rename-api/task.json` and `repo/`
- Create: `eval/tasks/split-validation/task.json` and `repo/`
- Modify: `core/tests/eval_test.rs`

**Interfaces:**
- Consumes: Task 2 `EvalTask` manifest fields and existing fixture-copy verifier.
- Produces: 12 deterministic tasks across `bugfix`, `parsing`, `api`, and `refactor`, with Rust, Python standard library, and Node standard library coverage.

- [ ] **Step 1: Write the failing fixture-integrity test**

```rust
#[test]
fn fixture_suite_has_twelve_valid_reference_solutions() {
    let root = repo_root().join("eval/tasks");
    let tasks = load_suite(&root, None).unwrap();
    assert_eq!(tasks.len(), 12);
    let categories = tasks.iter().map(|t| t.category.as_str()).collect::<HashSet<_>>();
    assert_eq!(categories, HashSet::from(["bugfix", "parsing", "api", "refactor"]));
    for task in tasks {
        assert!(["rust", "python", "node"].contains(&task.language.as_str()));
        assert!(["small", "medium"].contains(&task.difficulty.as_str()));
        assert!(task.repo_dir.parent().unwrap().join("solution.patch").is_file());
        assert_fixture_has_no_symlinks_or_build_output(&task.repo_dir).unwrap();
        assert_eq!(verify_fixture(&task.repo_dir, task.verify.as_ref()).unwrap(), VerificationStatus::Failed);
        let solved = apply_reference_patch(&task).unwrap();
        assert_eq!(verify_fixture(&solved, task.verify.as_ref()).unwrap(), VerificationStatus::Passed);
        assert_protected_paths_unchanged(&task, &solved).unwrap();
    }
}
```

- [ ] **Step 2: Run and confirm only four tasks/no patches exist**

Run: `cargo test -p consilium --test eval_test fixture_suite_has_twelve_valid_reference_solutions`

Expected: FAIL with task count `4` and missing `solution.patch`.

- [ ] **Step 3: Add metadata to the existing four manifests**

Use:

```json
{"add-greeting":{"category":"api","language":"rust","difficulty":"small"},
 "fix-sum":{"category":"bugfix","language":"rust","difficulty":"small"},
 "parse-duration":{"category":"parsing","language":"rust","difficulty":"medium"},
 "parse-kv":{"category":"parsing","language":"rust","difficulty":"small"}}
```

Merge the corresponding three fields into each existing manifest without changing its prompt, verifier, or protected paths.

- [ ] **Step 4: Add eight exact task contracts**

Create these tasks:

| Name | Category | Language | Difficulty | Contract | Verify |
|---|---|---|---|---|---|
| `normalize-email` | parsing | python | small | trim and lowercase one email, reject missing `@` | `python -m unittest` |
| `clamp-retry` | bugfix | node | small | clamp retries to integer range 0..10 | `node --test` |
| `json-content-type` | api | python | small | return JSON body plus `application/json` header | `python -m unittest` |
| `filter-active` | refactor | node | small | extract pure active-user filter without changing order | `node --test` |
| `path-extension` | parsing | rust | small | return lowercase final extension, excluding dotfiles | `cargo test` |
| `dedupe-stable` | refactor | rust | medium | remove duplicates while preserving first-seen order | `cargo test` |
| `rename-api` | api | node | medium | expose `formatUser` and keep deprecated `format_user` alias | `node --test` |
| `split-validation` | refactor | python | medium | separate parsing and validation, preserve public `load_config` | `python -m unittest` |

Each starter repo contains only source, package metadata, and a protected test; the baseline test fails for the intended assertion, not because a tool/runtime is missing.

- [ ] **Step 5: Generate and inspect reference patches**

For each task, make the minimal solution in a disposable copy, create `git diff --binary --full-index`, restore the starter, and save the reviewed patch as `solution.patch`. The patch may change only non-protected source files and package metadata required by the starter.

- [ ] **Step 6: Run fixture integrity and dry-run**

Run: `cargo test -p consilium --test eval_test fixture_suite && cargo run -q -p consilium -- eval`

Expected: PASS; dry-run lists 12 tasks and prints `no quota spent`.

- [ ] **Step 7: Commit**

```bash
git add eval/tasks core/tests/eval_test.rs
git commit -m "test: expand the zero-quota eval fixture suite"
```

### Task 4: Version, changelog, and release-contract validator

**Files:**
- Create: `script/validate_release.sh`
- Create: `script/test_validate_release.sh`
- Modify: `core/Cargo.toml`
- Modify: `desktop/src-tauri/Cargo.toml`
- Modify: `desktop/src-tauri/tauri.conf.json`
- Modify: `ui/package.json`
- Modify: `plugin/.claude-plugin/plugin.json`
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: versions from four package manifests and a requested tag.
- Produces: `script/validate_release.sh <tag>` with deterministic 0/nonzero behavior and no network.

- [ ] **Step 1: Write failing shell fixture tests**

```sh
#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cp -R "$ROOT/core" "$ROOT/desktop" "$ROOT/ui" "$ROOT/plugin" "$ROOT/CHANGELOG.md" "$TMP/"

"$ROOT/script/validate_release.sh" v0.3.0-beta "$TMP"
sed -i.bak 's/"version": "0.3.0-beta"/"version": "9.9.9"/' "$TMP/ui/package.json"
if "$ROOT/script/validate_release.sh" v0.3.0-beta "$TMP"; then
  echo "expected mismatched UI version to fail" >&2
  exit 1
fi
if "$ROOT/script/validate_release.sh" release-0.3 "$TMP"; then
  echo "expected malformed tag to fail" >&2
  exit 1
fi
```

- [ ] **Step 2: Run and confirm validator is absent**

Run: `sh script/test_validate_release.sh`

Expected: FAIL with `script/validate_release.sh: not found`.

- [ ] **Step 3: Set consistent beta versions and package policy**

Set `0.3.0-beta` in CLI Cargo, desktop Cargo/Tauri, UI, and plugin manifests. Add to `core/Cargo.toml`:

```toml
homepage = "https://github.com/TemurTurayev/consilium"
readme = "../README.md"
rust-version = "1.85"
publish = false
```

Set `publish = false` in the desktop Cargo package too. Add a `## [0.3.0-beta] - Unreleased` changelog heading with Added/Changed/Security sections matching implemented behavior.

- [ ] **Step 4: Implement a POSIX validator**

The script accepts tag and optional root, requires `vMAJOR.MINOR.PATCH` with optional `-beta`, strips `v`, extracts versions using `cargo metadata` plus `sed` for JSON manifests, verifies exact equality, requires the matching changelog heading, and rejects a dirty generated cargo-dist workflow during CI. Every error names the mismatched file and values.

- [ ] **Step 5: Run syntax and fixture tests**

Run: `sh -n script/validate_release.sh script/test_validate_release.sh && sh script/test_validate_release.sh && script/validate_release.sh v0.3.0-beta`

Expected: PASS; intentionally mismatched/malformed fixtures fail inside the test script.

- [ ] **Step 6: Commit**

```bash
git add script/validate_release.sh script/test_validate_release.sh core/Cargo.toml desktop/src-tauri/Cargo.toml desktop/src-tauri/tauri.conf.json ui/package.json plugin/.claude-plugin/plugin.json CHANGELOG.md
git commit -m "build: enforce the v0.3 release contract"
```

### Task 5: Pinned cargo-dist plan and fail-closed installer

**Files:**
- Create: `dist-workspace.toml`
- Modify: `Cargo.toml`
- Modify: `.github/workflows/release.yml`
- Modify: `install.sh`
- Create: `script/test_install.sh`

**Interfaces:**
- Consumes: cargo-dist 0.32.0 and existing Tauri build commands.
- Produces: three CLI archives/checksums, shell installer, GitHub-attached Homebrew formula, desktop artifacts, and a checksum-required compatibility installer.

- [ ] **Step 1: Write a failing offline installer test**

```sh
#!/bin/sh
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin"
cat >"$TMP/bin/uname" <<'EOF'
#!/bin/sh
[ "${1:-}" = "-s" ] && echo Linux || echo x86_64
EOF
cat >"$TMP/bin/curl" <<'EOF'
#!/bin/sh
case "$*" in *sha256*) exit 22 ;; *) : >"${4}" ;; esac
EOF
cat >"$TMP/bin/tar" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$TMP/bin/chmod" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$TMP/bin/uname" "$TMP/bin/curl" "$TMP/bin/tar" "$TMP/bin/chmod"
if PATH="$TMP/bin:$PATH" HOME="$TMP/home" sh "$ROOT/install.sh"; then
  echo "installer must fail when checksum is unavailable" >&2
  exit 1
fi
```

- [ ] **Step 2: Run and confirm current installer incorrectly succeeds past checksum retrieval**

Run: `sh script/test_install.sh`

Expected: FAIL because current installer prints `skipping verification` instead of aborting at the checksum boundary.

- [ ] **Step 3: Add the pinned dist configuration**

```toml
[dist]
cargo-dist-version = "0.32.0"
ci = ["github"]
installers = ["shell", "homebrew"]
targets = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
]
pr-run-mode = "plan"
publish-jobs = []
unix-archive = ".tar.gz"
checksum = "sha256"
packages = ["consilium"]
```

Do not add a tap, crates.io publisher, signing secret, or desktop package to this file.

- [ ] **Step 4: Generate and inspect cargo-dist output with approval if installation is needed**

First run: `dist --version`.

Expected: `cargo-dist 0.32.0`. If the binary is absent or another version is installed, request approval before `cargo install cargo-dist --version 0.32.0 --locked`; do not download silently.

Then run:

```bash
dist plan --tag=v0.3.0-beta
dist manifest --artifacts=local --no-local-paths
dist manifest --artifacts=global --no-local-paths
dist generate
```

Expected: exactly three CLI archives with SHA-256 checksums, one shell installer, one Homebrew formula attached as a global release artifact, and no `consilium-desktop` cargo-dist application.

- [ ] **Step 5: Coordinate the generated CLI workflow with desktop artifacts**

Keep cargo-dist's generated CLI jobs intact. Add a desktop job that depends on the same planning gate and uploads macOS/Linux Tauri bundles to the same draft release only after CLI artifacts succeed. A final publish job depends on all artifact jobs; it may publish only on a real pushed tag and must never run in pull-request plan mode. Keep `contents: write` scoped to the publish job.

- [ ] **Step 6: Make `install.sh` fail closed**

Replace the optional checksum branch with:

```sh
checksum_url="$url.sha256"
curl -fsSL "$checksum_url" -o "$tmpdir/consilium-$target.tar.gz.sha256" || {
  echo "consilium: checksum is unavailable — refusing to install an unverified binary" >&2
  exit 1
}
echo "Verifying checksum…"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmpdir" && sha256sum -c "consilium-$target.tar.gz.sha256" >/dev/null)
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmpdir" && shasum -a 256 -c "consilium-$target.tar.gz.sha256" >/dev/null)
else
  echo "consilium: no SHA-256 verifier found — refusing installation" >&2
  exit 1
fi
```

- [ ] **Step 7: Validate release plan and installer without publishing**

Run: `sh -n install.sh script/test_install.sh && sh script/test_install.sh && dist plan --tag=v0.3.0-beta && git diff --check`

Expected: PASS; no tag, release, package, or tap is created.

- [ ] **Step 8: Commit**

```bash
git add dist-workspace.toml Cargo.toml .github/workflows/release.yml install.sh script/test_install.sh
git commit -m "build: prepare verified CLI and desktop artifacts"
```

### Task 6: CI gates, audited historical evidence, and repository topics

**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `docs/eval/historical-v0.2.0-n1.md`
- Modify: `README.md`
- Modify: `.gitignore`

**Interfaces:**
- Consumes: Tasks 1–5 tests, schema v2, fixture suite, release validator, and dist plan.
- Produces: full local/CI gate, honest historical report, and discoverable repository metadata.

- [ ] **Step 1: Add the missing CI gates**

Add UI `npm run build` after tests. Add a `contracts` job with Rust available that runs:

```yaml
- name: Validate fixture suite
  run: cargo test -p consilium --test eval_test fixture_suite
- name: Prove eval dry-run is quota-free
  run: cargo run -q -p consilium -- eval | tee /tmp/eval-dry-run.txt
- name: Check dry-run claim
  run: grep -F "no quota spent" /tmp/eval-dry-run.txt
- name: Validate release versions
  run: script/validate_release.sh v0.3.0-beta
- name: Plan release artifacts
  run: dist plan --tag=v0.3.0-beta
```

Install the pinned cargo-dist binary from its official release action or locked installer in that job; pin the action/version, not `latest`.

- [ ] **Step 2: Create an aggregate-only historical evidence page**

Use this heading and disclaimer verbatim:

```markdown
# Historical v0.2.0 evaluation: N=1, four small Rust tasks

This page preserves one historical local run for transparency. It is not a benchmark claim: N=1 has no variance estimate, the suite contains only four small Rust tasks, provider CLIs and model versions can change, and the raw v0.2 files used an older token schema. The result must not be generalized to other languages, repositories, models, or task sizes.
```

Record source file `eval/results/1782215402-results.json`, local file date `2026-06-23`, the Consilium commit stored alongside the run if present (otherwise state `commit not recorded by schema v1`), and these audited aggregates:

| Approach | Passed | Claude tokens | Codex tokens | Gemini tokens | Grok tokens | Summed wall time |
|---|---:|---:|---:|---:|---:|---:|
| `solo` | 4/4 | 967,688 | 0 | 0 | not recorded | 183,929 ms |
| `conduct` | 4/4 | 307,723 | 346,534 | 1,794 | not recorded | 375,054 ms |
| `conduct-no-grounding` | 4/4 | 308,002 | 311,409 | 1,799 | not recorded | 369,291 ms |

State that each number is the sum of four N=1 task records and that the v1 token counters may be heuristic/provider-dependent. Name the four tasks: `add-greeting`, `fix-sum`, `parse-duration`, and `parse-kv`. Do not commit raw prompts/transcripts or ignored result JSON. Link this page from README under `Evaluation evidence`, next to `eval/README.md` for reproduction instructions.

- [ ] **Step 3: Preserve raw-result privacy in `.gitignore`**

Keep `eval/results/` and transcript directories ignored. Add a comment that only reviewed aggregate documents under `docs/eval/` are publishable evidence.

- [ ] **Step 4: Run the complete local acceptance gate**

```bash
cargo fmt --check
cargo clippy -p consilium --all-targets -- -D warnings
cargo test -p consilium
npm --prefix ui ci
npm --prefix ui run typecheck
npm --prefix ui test
npm --prefix ui run build
sh -n install.sh script/validate_release.sh script/test_validate_release.sh script/test_install.sh
sh script/test_validate_release.sh
sh script/test_install.sh
script/validate_release.sh v0.3.0-beta
dist plan --tag=v0.3.0-beta
git diff --check
```

Expected: every command passes; eval dry-run spends no quota and no release/tag is created.

- [ ] **Step 5: Apply repository topics after verifying the remote**

Run: `gh repo view TemurTurayev/consilium --json nameWithOwner,url`

Expected: exactly `TemurTurayev/consilium`. Then run:

```bash
gh repo edit TemurTurayev/consilium \
  --add-topic multi-agent \
  --add-topic ai-agents \
  --add-topic claude-code \
  --add-topic codex-cli \
  --add-topic gemini \
  --add-topic llm-orchestration \
  --add-topic code-review \
  --add-topic mcp \
  --add-topic rust
```

Verify with: `gh repo view TemurTurayev/consilium --json repositoryTopics`.

Expected: all nine topics are present. This changes only reversible repository metadata; do not push code, create issues, tags, or releases in this step.

- [ ] **Step 6: Create three concrete good-first-issue tickets**

Create or update the `good first issue` label, then create exactly these issues after checking with `gh issue list --search '<title> in:title'` that they do not already exist:

```bash
gh label create "good first issue" --repo TemurTurayev/consilium --color 7057ff --description "Small, well-scoped contribution with acceptance criteria" --force
gh issue create --repo TemurTurayev/consilium --label "good first issue" --title "Add a zero-quota Go task to the eval fixture suite" --body $'Add one deterministic Go standard-library fixture under `eval/tasks/` following `eval/README.md`.\n\nAcceptance:\n- starter `go test ./...` fails for the intended assertion\n- protected tests are unchanged\n- `solution.patch` makes the fixture pass\n- fixture integrity tests stay green\n- no provider quota is used'
gh issue create --repo TemurTurayev/consilium --label "good first issue" --title "Add screen-reader descriptions to the Table role seats" --body $'Improve the existing medical Table without changing its visual identity.\n\nAcceptance:\n- every role seat exposes role, provider, and state through an accessible name\n- paused, running, success, and failed states are announced in text\n- keyboard navigation remains usable\n- add a focused Testing Library test\n- UI typecheck, tests, and build pass'
gh issue create --repo TemurTurayev/consilium --label "good first issue" --title "Document one custom provider adapter end to end" --body $'Add a contributor walkthrough for a hypothetical provider adapter; do not add a real provider.\n\nAcceptance:\n- names the adapter, catalog, auth, event parsing, and test files to touch\n- includes a zero-quota scripted-adapter test example\n- explains advisory versus write permissions\n- links from `CONTRIBUTING.md`\n- contains no real credentials or provider claims'
```

Expected: exactly three open, non-duplicate issues with bounded acceptance criteria. This public metadata work is reversible; do not create a release, tag, or PR in this step.

- [ ] **Step 7: Commit file-backed final corrections**

```bash
git add .github/workflows/ci.yml docs/eval/historical-v0.2.0-n1.md README.md .gitignore
git commit -m "ci: verify community release evidence"
```

Skip this commit when all file-backed changes were already committed in prior tasks.

### Task 7: Non-publication handoff

**Files:**
- No file changes expected.

**Interfaces:**
- Consumes: green Tasks 1–6 and current Git status.
- Produces: a precise operator handoff without publishing.

- [ ] **Step 1: Confirm repository cleanliness without touching user files**

Run: `git status --short --branch`

Expected: implementation files are committed; user-owned `.claude/` may remain untracked and must remain unstaged.

- [ ] **Step 2: Confirm publication did not occur**

Run: `git tag --points-at HEAD && gh release view v0.3.0-beta --repo TemurTurayev/consilium`

Expected: no local tag points at HEAD and GitHub reports no `v0.3.0-beta` release. Treat that absence as success for this preparation plan.

- [ ] **Step 3: Report the separately authorized next operations**

The handoff lists, but does not execute: push implementation branch/PR, merge, create and push `v0.3.0-beta`, monitor artifact/signing checks, publish the draft release, create an external Homebrew tap, enable Private Vulnerability Reporting, and create labelled `good first issue` tickets.
