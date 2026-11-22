# Consilium

> One task. Multiple AI coding agents. One reviewed result.

Consilium lets Claude Code, Codex, Gemini and Grok work together from the
subscriptions you already have. It runs their official CLIs locally, gives each
agent a role, and keeps the prompts, diffs and results on your machine.

Use it when one model's answer is not enough:

- **Ask a council** — get independent answers, anonymous peer review and a final synthesis.
- **Review a diff** — have another model audit the change before you merge it.
- **Build with a team** — let a conductor split a coding task between workers, run your tests and review the result.

No provider API keys. No hosted orchestration service. Consilium is open source
and works with the CLI subscriptions you already pay for.

## Try it in two minutes

You need macOS or Linux and at least one supported agent CLI already installed
and signed in: `claude`, `codex`, `agy` (Gemini), or experimental `grok`
(Grok CLI 1.0.13 or newer — xAI refuses older ones; `grok update`).

```sh
curl -fsSL https://raw.githubusercontent.com/TemurTurayev/consilium/main/install.sh | sh
consilium init
consilium council "What is the safest way to simplify this module?"
```

`consilium init` detects the providers available on your machine, explains any
missing login step, finds the best model each of your subscriptions can run and
creates `consilium.config.json`. It takes a couple of minutes: every provider is
asked a one-word question to prove it answers.

If a run later reports that a model is unavailable (providers rename and retire
models every few months), let Consilium switch to current ones:

```sh
consilium models --write
```

Not ready to connect a provider? You can explore the interface with a built-in
demo that does not start agents or spend quota:

```sh
git clone https://github.com/TemurTurayev/consilium.git
cd consilium/ui
npm install
npm run dev
```

Open the local address printed by Vite and choose **Try the demo**.

## Three useful commands

### Get a second opinion

```sh
consilium council "Should this service use a queue or stay synchronous?"
```

Agents answer independently, review anonymized answers from the others, and a
chairman combines the strongest parts into one response.

### Review code

```sh
git diff | consilium review --diff-file /dev/stdin
```

The reviewer focuses on concrete problems in the diff. The command returns a
non-zero exit code for critical findings or an unparseable review, so it can be
used in scripts and CI.

### Delegate a coding task

```sh
consilium conduct "Add validation to the settings form and cover it with tests"
```

The conductor decomposes the task, workers edit the repository, configured
checks verify the change, and another model reviews it. Run this in a Git
repository and inspect the resulting diff as you would after working with a
human collaborator.

For a full triage-to-verification pipeline:

```sh
consilium auto "Fix the failing parser test" --check "cargo test"
```

## How it works

```text
Your task
   |
   v
Conductor or chairman
   |
   +--> Claude Code
   +--> Codex CLI
   +--> Gemini via agy
   +--> Grok Build CLI (experimental)
   |
   v
Cross-review + local checks
   |
   v
Result, diff and transcript on your machine
```

Consilium does not proxy model APIs. It launches supported provider CLIs as
local processes and uses the authentication those tools already manage.

## Install

### Prebuilt binary

macOS (Apple Silicon and Intel) and Linux x86_64:

```sh
curl -fsSL https://raw.githubusercontent.com/TemurTurayev/consilium/main/install.sh | sh
```

The script installs to `~/.local/bin/consilium` and tells you if that directory
must be added to `PATH`.

### With Rust

```sh
cargo install --git https://github.com/TemurTurayev/consilium consilium
```

### From source

```sh
git clone https://github.com/TemurTurayev/consilium.git
cd consilium
cargo build --release
```

On Windows, use WSL. Native Windows is not supported yet.

## Everyday commands

| Command | Purpose |
|---|---|
| `consilium init` | Detect providers and create a configuration (`--global` writes your own council for every directory) |
| `consilium doctor` | Check that the provider CLIs are installed (`--models` also probes every configured model) |
| `consilium auth` | Show provider authentication state and next steps |
| `consilium run --provider codex "..."` | Send one prompt to one provider |
| `consilium council "..."` | Deliberate on a question with multiple agents |
| `consilium review` | Audit the current Git diff |
| `consilium conduct "..."` | Split, implement, verify and review a coding task |
| `consilium auto "..." --check "..."` | Run the complete automated pipeline |
| `consilium quota` | Show local provider usage counters |
| `consilium models` | Find the best model each provider can run on your account (`--write` updates the configuration) |
| `consilium serve` | Start the localhost server for the web interface |
| `consilium mcp` | Run the MCP server for an attached conductor |

Use `consilium <command> --help` for all options.

## Configuration

The onboarding wizard writes a complete `consilium.config.json` with current
model choices. Most users can leave it alone. To ground coding runs in your own
checks, add or edit its `verify` section:

```json
"verify": {
  "test": "cargo test",
  "build": "cargo build"
}
```

The snippet above is one section of the generated file, not a complete
configuration. Start with the wizard's defaults and customize roles only when
you need to.

Configuration is read in layers, each overriding the one before it:

1. the built-in lineup;
2. `~/.consilium/consilium.config.json` — your own council, which follows you
   into every directory (`consilium init --global` writes it);
3. `./consilium.config.json` — what one project changes. It can be a whole
   configuration or only the keys that differ, such as the `verify` section
   above; inside `roles` it overrides role by role.

Every key layers this way, not only `roles`: a `verify` in your own file would
run in every project, so keep build and test commands in the project's file.
`consilium doctor --models` prints which files were read.

Each role names a provider and a model, plus optional `fallbacks` tried in
order when the model is unavailable or rate-limited. The model name `default`
means "let the provider's CLI choose": it never goes out of date and always
fits your plan, so the generated ladders end with it.

```json
"reviewer": {
  "provider": "codex",
  "model": "gpt-6.1-sol",
  "fallbacks": [{ "provider": "codex", "model": "default" }]
}
```

## Safety and privacy

- Provider credentials stay with the official provider CLIs; Consilium does not
  ask for API keys. The one thing it reads is `~/.consilium/secrets.env`: the
  headless Claude CLI signs in with the token from `claude setup-token`, and a
  line `export CLAUDE_CODE_OAUTH_TOKEN=<token>` there reaches it wherever
  Consilium runs — including under a desktop app, which never loads your shell
  profile. Keep that file private (`chmod 600`).
- A provider CLI that is not signed in is never waited on: the run fails at
  once, that provider is skipped for the rest of it, and `consilium auth`
  says how to sign in. Consilium does not open a browser sign-in for you.
- Prompts, transcripts and diffs stay local unless a provider CLI transmits
  them to its own service as part of a run.
- Build and test commands execute on your machine. Review configuration before
  running a repository you do not trust.
- A provider CLI may start helpers of its own, such as MCP servers. Consilium
  stops everything a run started when the run ends, is cancelled or times out,
  and when Consilium itself is interrupted.
- Coding agents can make mistakes. Review the diff and test the result before
  merging or deploying it.

## Web interface

Start the local server:

```sh
consilium serve
```

Then run the UI in a second terminal:

```sh
cd ui
npm install
npm run dev
```

The interface shows a live session, provider activity and quota information.
The **Try the demo** button works without the backend.

## Project status

Consilium is beta software. The council, review, conduct, auto, MCP and local
web flows are implemented and tested, but the command surface and configuration
may still change.

Current platform support:

- macOS: supported
- Linux x86_64: supported
- Windows through WSL: supported path
- Native Windows: not yet supported
- Grok provider: experimental

See [releases](https://github.com/TemurTurayev/consilium/releases) for packaged
builds and release notes. Internal design notes and implementation plans live in
[`docs/`](docs/); they are useful for contributors, but not required to use the
tool.

## Development

```sh
cargo test --workspace
cd ui && npm test
```

Bug reports and focused pull requests are welcome. If you are proposing a large
change, open an issue first so the design can be discussed before implementation.

## License

[MIT](LICENSE) © 2026 Temur Turayev
