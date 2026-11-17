# Consilium eval

`consilium eval` measures whether Consilium's orchestration beats one model
working alone. It runs each task in the suite through one or more *approaches*,
then scores what each approach left behind with the task's own build and tests.

## The quota boundary

A live run calls real provider CLIs and spends your subscription limits. It
never happens by accident:

```sh
consilium eval
```

prints the plan — tasks, approaches, rough call counts — and the words
`no quota spent`. It calls no model, opens no configuration and writes no file.
A live run needs the explicit flag:

```sh
consilium eval --spend-quota
```

Tests and CI never pass `--spend-quota`. Everything under `cargo test` uses
scripted stand-ins for the provider CLIs.

## Running it

```sh
# What would run, and roughly how many model calls (spends nothing)
consilium eval --approaches solo,conduct --trials 3

# One task, both approaches, three trials each (spends quota)
consilium eval --task parse-kv --approaches solo,conduct --trials 3 --spend-quota
```

| Option | Default | Meaning |
|---|---|---|
| `--suite <dir>` | `eval/tasks` | Directory of tasks |
| `--approaches <list>` | `solo,conduct` | Any of `solo`, `conduct`, `conduct-no-grounding`, `conduct-cross-family` |
| `--trials <N>` | `1` | Trials per task and approach |
| `--task <text>` | all | Only tasks whose name contains the text |
| `--timeout <seconds>` | `900` | Hard limit per model session |
| `--out <file>` | `eval/results/<unix-time>-results.json` | Where the JSON report goes |
| `--spend-quota` | off | Actually call models |

The report file is rewritten after every finished trial, so a run that is
stopped or dies part-way keeps everything it has measured; such a file is
marked partial (see `planned_runs` below).

A live run reads `consilium.config.json` from the current directory. `solo` is
the conductor's model alone on the whole task; the `conduct` approaches use the
full lineup. Results in `eval/results/` are local and git-ignored: publish
figures only as a reviewed summary that states the date, suite commit, N and
limits.

## How a trial is scored

1. The task's `repo/` is copied to a fresh temporary directory and committed.
2. The approach runs there, with its own in-memory usage ledger.
3. The task's protected files (its tests) are restored from that first commit,
   so an approach cannot pass by editing the test it is judged on.
4. The task's `verify` commands run. Their result is the score.

The approach's own "done" is recorded as `pipeline_ok` but is never the score.

## Report schema v2

The JSON report has `"schema_version": 2`. Version 1 files (before 2026-11) have
no version field and are not read by any command.

| Field | Meaning |
|---|---|
| `schema_version` | `2` |
| `generated_at` | UTC time the run started, e.g. `2026-11-17T12:00:00Z` |
| `suite_commit` | Commit of the repository holding the suite; `-dirty` is appended if the suite had uncommitted changes; `unknown` outside git |
| `trials` | N requested per task and approach |
| `planned_runs` | Runs the whole matrix calls for (tasks × approaches × trials); if `results` holds fewer, the report is partial |
| `results[]` | One entry per trial (below) |
| `aggregate` | `per_task_approach`, `per_category_approach`, `per_approach` |

Each trial:

| Field | Meaning |
|---|---|
| `task`, `category`, `language`, `difficulty` | Copied from the task manifest |
| `approach`, `trial` | Which approach, and the trial's index from 0 |
| `verification` | `passed`, `failed` or `not_run` — the score |
| `pipeline_ok` | The approach's own claim of completion; informational |
| `attempts` | Worker passes: 1 for `solo`; first tries plus reworks for `conduct` |
| `fallbacks` | Times a model failed and the run moved to the next one |
| `tokens` | `claude`, `codex`, `gemini`, `grok` (input + output) |
| `wall_ms` | Time the approach took, scoring excluded |
| `error` | Present only if the approach aborted |

`verification` has three states on purpose. `not_run` means no verifier command
could be resolved, so the trial was not scored. It is never counted as a pass,
and it is reported apart from real failures.

Each aggregate row carries `passed`, `failed`, `not_run`, `total`, `stable`,
summed `attempts` and `fallbacks`, and a summary of `wall_ms` and of each
provider's tokens:

```json
{ "n": 3, "mean": 5120.0, "median": 4980.0, "std_dev": 410.2 }
```

## Reading the statistics

- `n` is always stated. A figure without its sample size is not evidence.
- `std_dev` is the sample standard deviation (`n - 1` denominator). It is
  `null` when `n` is 1: one run says nothing about run-to-run variation.
- `stable` says whether every trial of a task reached the same verification
  state. It is `null` for a single trial.
- In `per_task_approach` rows the spread is between repeated runs. In
  `per_approach` and `per_category_approach` rows it also includes the
  difference between tasks, so do not read it as noise.
- Gemini runs through the Antigravity `agy` CLI, which reports no usage. Its
  token counts are estimates (about four characters per token) and are not
  directly comparable to the measured Claude and Codex counts.
- A result holds for this suite, at this commit, at this N. It is not a
  general claim about the models.

## The built-in suite

Twelve small, deterministic tasks. They use only each language's standard
library, so a trial needs no network and no dependency install.

| Task | Category | Language | Difficulty | What is asked |
|---|---|---|---|---|
| `add-greeting` | api | rust | small | Add a public function the test calls |
| `clamp-retry` | bugfix | node | small | Clamp a retry count to a whole number from 0 to 10 |
| `dedupe-stable` | refactor | rust | medium | Extract a generic, order-preserving de-duplication |
| `filter-active` | refactor | node | small | Extract a pure active-user filter; keep the report unchanged |
| `fix-sum` | bugfix | rust | small | Fix an off-by-one in an inclusive sum |
| `json-content-type` | api | python | small | Return a JSON body with its `Content-Type` header |
| `normalize-email` | parsing | python | small | Trim and lowercase an address; reject one without `@` |
| `parse-duration` | parsing | rust | medium | Parse `1h30m`-style durations with strict validation |
| `parse-kv` | parsing | rust | small | Split `key=value` on the first `=` |
| `path-extension` | parsing | rust | small | Lowercase final extension; dotfiles have none |
| `rename-api` | api | node | medium | Rename a function and keep a deprecated alias |
| `split-validation` | refactor | python | medium | Separate parsing from validation behind the same entry point |

That is three tasks per category; six in Rust, three in Python, three in Node.
They are deliberately small. The suite can show whether an approach breaks
easy work or wastes tokens on it; it cannot show how approaches compare on
large or ambiguous tasks.

### The suite checks itself

Every task ships a reference `solution.patch`. A test applies it and proves,
for each task, without calling any model:

- the starter repository **fails** the task's own check;
- with the reference patch applied, the same check **passes**;
- the patch does not touch the protected tests.

So each task is solvable, is not already solved, and fails because of the
code rather than a missing tool. The same test also feeds the check
deliberately broken fixtures and requires it to object.

```sh
cargo test -p consilium --test eval_fixture_test
```

It needs `cargo`, `python3` and `node` (18 or newer) on `PATH` and takes a few
seconds. Live trials need the same tools, because they run the same checks.

## Task format

Each task is a directory under `eval/tasks/`:

```text
eval/tasks/<name>/
  task.json        the manifest
  repo/            the starter repository, copied for every trial
  solution.patch   a reference solution, used only by the integrity test
```

`solution.patch` sits outside `repo/`, so it is never copied into a trial and
no model sees it.

```json
{
  "name": "parse-kv",
  "category": "parsing",
  "language": "rust",
  "difficulty": "small",
  "prompt": "What the model is asked to do.",
  "context": "Optional background for the conductor.",
  "verify": { "build": "cargo build", "test": "cargo test" },
  "protected_paths": ["tests/parse.rs"]
}
```

`name`, `category`, `language`, `difficulty` and `prompt` are required.
`verify` may hold `build`, `test`, `lint` and `timeoutSecs`; `build` and `test`
decide the score, `lint` is advisory. `protected_paths` lists the files that
are restored before scoring — normally the tests.

### Adding a task

1. Create `eval/tasks/<name>/repo/` with the source, package metadata and a
   test. Use only the standard library. The test must fail on the starter for
   the reason the task describes.
2. Write `task.json`. List the test under `protected_paths`.
3. Solve the task in a throwaway copy of `repo/` that you have committed with
   git, and save the result of `git diff --binary --full-index` as
   `eval/tasks/<name>/solution.patch`. Change only non-protected files.
4. Add the task's name to `TASK_NAMES` in `core/tests/eval_fixture_test.rs` and
   run the integrity test above.

Do not run a task's tests inside `eval/tasks/<name>/repo/` itself: build output
left there (`target/`, `__pycache__/`, `node_modules/`) fails the integrity
test. Copy the directory somewhere else first.
