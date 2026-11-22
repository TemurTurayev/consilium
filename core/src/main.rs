use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "consilium",
    version,
    about = "Multi-agent orchestrator for subscription CLI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check that agent CLIs are installed and authenticated
    Doctor {
        /// Also probe each configured model's availability (spends a tiny amount
        /// of provider quota — one "Reply with: ok" call per distinct model).
        #[arg(long)]
        models: bool,
    },
    /// Report each provider's auth state and the exact next step to authenticate
    /// it (probes liveness — spends ~1 token per provider).
    Auth {
        /// Probe just one provider (claude|codex|gemini|grok) instead of all.
        #[arg(long)]
        provider: Option<String>,
    },
    /// Run a single prompt through one agent (smoke test)
    Run {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: Option<String>,
        prompt: String,
    },
    /// Show usage counters per provider
    Quota,
    /// Convene the council: independent answers, anonymized cross-review, synthesis
    Council {
        question: String,
        /// Hard per-session timeout in seconds
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Audit a git diff with the reviewer role
    Review {
        /// Review staged changes instead of unstaged
        #[arg(long)]
        staged: bool,
        /// Read the diff from a file instead of running git
        #[arg(long)]
        diff_file: Option<std::path::PathBuf>,
        /// Who wrote the diff ("provider/model"). Needed for a refutation to
        /// lift a critical finding: nobody can be shown independent of an
        /// author that was not named.
        #[arg(long)]
        author_model: Option<String>,
        /// Do not have a second model rule on the findings
        #[arg(long)]
        no_cross_examine: bool,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// How each model has done so far, counted from the saved runs
    Standings {
        /// Count only runs saved in the last N days (0 = every saved run)
        #[arg(long, default_value_t = 90)]
        days: u64,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Conduct a coding task: conductor decomposes → workers execute → review gate
    Conduct {
        task: String,
        /// Additional context for the conductor (e.g. relevant architecture notes)
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        no_preflight: bool,
        /// Hard per-session timeout in seconds
        #[arg(long, default_value_t = 900)]
        timeout: u64,
    },
    /// Auto pipeline: triage → (council) → conduct → optional check command
    Auto {
        task: String,
        /// Shell command to run after conduct completes (exit code determines success)
        #[arg(long)]
        check: Option<String>,
        #[arg(long)]
        no_preflight: bool,
        /// Hard per-session timeout in seconds
        #[arg(long, default_value_t = 900)]
        timeout: u64,
    },
    /// Set up consilium.config.json. With no flags, runs the interactive
    /// onboarding wizard; --yes writes the recommended lineup non-interactively.
    Init {
        /// Overwrite an existing consilium.config.json without asking.
        #[arg(long)]
        force: bool,
        /// Skip the wizard: write the recommended council non-interactively (CI/scripts).
        #[arg(long)]
        yes: bool,
        /// Write your own council to ~/.consilium/consilium.config.json, which
        /// applies in every directory, instead of this project's file.
        #[arg(long)]
        global: bool,
    },
    /// Find the best model each provider can run on your account (a few
    /// ~1-token probes per provider) and, with --write, update
    /// consilium.config.json so retired or plan-gated models switch to it.
    /// Run it when a run reports an unavailable model.
    Models {
        /// Rewrite consilium.config.json with the models this account can run.
        #[arg(long)]
        write: bool,
    },
    /// Run as an MCP server over stdio (attached-conductor mode). Register this
    /// in your interactive Claude Code session to drive workers via the
    /// `run_worker` and `quota_status` tools without spending Claude credit.
    Mcp,
    /// Run the localhost server. Open a WebSocket at /ws/session and send a
    /// `{"kind":"conduct","task":"..."}` frame to stream a run's events live.
    Serve {
        /// Address to bind (host:port).
        #[arg(long, default_value = "127.0.0.1:7878")]
        addr: String,
        /// Hard per-session timeout in seconds.
        #[arg(long, default_value_t = 900)]
        timeout: u64,
    },
    /// Benchmark orchestration approaches by build/test pass-rate over a task
    /// suite. Dry-runs by default (spends nothing); pass --spend-quota to run.
    Eval {
        /// Directory of tasks (each subdir has a task.json + repo/).
        #[arg(long, default_value = "eval/tasks")]
        suite: std::path::PathBuf,
        /// Comma-separated approaches: solo,conduct,conduct-no-grounding,conduct-cross-family
        #[arg(long, default_value = "solo,conduct")]
        approaches: String,
        /// Trials per (task, approach). Live models are nondeterministic.
        #[arg(long, default_value_t = 1)]
        trials: u32,
        /// Results JSON output path (default: eval/results/<ts>-results.json).
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Only run tasks whose name contains this substring.
        #[arg(long)]
        task: Option<String>,
        /// Hard per-session timeout in seconds.
        #[arg(long, default_value_t = 900)]
        timeout: u64,
        /// REQUIRED to actually spend provider quota. Without it, eval dry-runs.
        #[arg(long)]
        spend_quota: bool,
    },
}

fn print_welcome() {
    println!(
        r#"Consilium — your AI coding council.
One command, several AI agents: your smartest model plans and reviews,
cheaper models write the code — all on the subscriptions you already pay for.

New here?
  consilium init          set up your council (takes ~2 minutes)

Then try your first task:
  consilium conduct "add a function that reverses a string, with a test"

Or ask your council a question:
  consilium council "what's the cleanest way to handle errors in Rust?"

More:  auth · doctor · quota · review · auto · serve
Run `consilium <command> --help` for details on any command."#
    );
}

fn quota_db_path() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("$HOME is not set; cannot locate ~/.consilium/usage.db"))?;
    Ok(std::path::PathBuf::from(home)
        .join(".consilium")
        .join("usage.db"))
}

/// `base` with the keys of `added` laid over it.
fn merged(base: &serde_json::Value, added: &serde_json::Value) -> serde_json::Value {
    let fields = |value: &serde_json::Value| value.as_object().cloned().unwrap_or_default();
    serde_json::Value::Object(fields(base).into_iter().chain(fields(added)).collect())
}

/// Print a one-line, free (no-probe) hint on stderr if the config pins any model
/// the catalog has superseded — shown before council/conduct/auto runs so the
/// operator always knows when a newer model is available.
fn print_staleness_hint(config: &consilium::config::Config) {
    let stale = consilium::models::stale_models(config);
    if !stale.is_empty() {
        let names: Vec<String> = stale
            .iter()
            .map(|s| format!("{}/{}", s.provider.as_str(), s.current))
            .collect();
        eprintln!(
            "ℹ retired model(s) in config: {} — run `consilium models --write` to switch to current ones.",
            names.join(", ")
        );
    }
}

/// Provider CLIs run in process groups of their own (see `consilium::reaper`),
/// so a Ctrl-C in the terminal — or a `kill`, or the terminal closing — reaches
/// only consilium. Stop every provider run still in flight before exiting, with
/// the exit code a shell reports for that signal.
#[cfg(unix)]
fn stop_providers_when_signalled() {
    use tokio::signal::unix::{signal, SignalKind};
    let signals = [
        (SignalKind::interrupt(), 130),
        (SignalKind::terminate(), 143),
        (SignalKind::hangup(), 129),
    ];
    for (kind, exit_code) in signals {
        let Ok(mut received) = signal(kind) else {
            continue;
        };
        tokio::spawn(async move {
            if received.recv().await.is_some() {
                consilium::reaper::kill_all();
                std::process::exit(exit_code);
            }
        });
    }
}

#[cfg(not(unix))]
fn stop_providers_when_signalled() {}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    stop_providers_when_signalled();
    // Logs go to stderr so they never corrupt the MCP server's stdout JSON-RPC
    // framing (`consilium mcp`); benign for every other subcommand.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            print_welcome();
            return Ok(());
        }
    };
    match command {
        Command::Doctor { models } => {
            let mut all_ok = true;
            // check_with_path uses std::process::Command (blocking); acceptable for a
            // one-shot CLI diagnostic — no async work runs concurrently with this arm.
            for status in consilium::doctor::run_doctor() {
                if status.found {
                    println!(
                        "✓ {:8} {}",
                        status.binary,
                        status.version.unwrap_or_default()
                    );
                } else {
                    all_ok = false;
                    println!("✗ {:8} not found", status.binary);
                }
            }
            if !all_ok {
                println!("\nInstall missing CLIs:");
                println!("  codex:  npm install -g @openai/codex   (then: codex login)");
                println!("  agy:    the Antigravity CLI (replaces the gemini CLI) — https://antigravity.google");
                println!("  claude: see https://code.claude.com");
                std::process::exit(1);
            }

            if models {
                let files = consilium::config_files::ConfigFiles::locate(&std::env::current_dir()?);
                let config = files.load()?;
                println!(
                    "config: {}",
                    files
                        .describe()
                        .unwrap_or_else(|| "built-in defaults (no config file)".to_string())
                );
                let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
                let report = consilium::doctor::preflight(&config, &store).await;
                consilium::doctor::print_preflight(&report);
                if !report.all_ok() {
                    std::process::exit(1);
                }
            }
        }
        Command::Auth { provider } => {
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let report = match provider {
                Some(name) => {
                    let p: consilium::event::Provider = name
                        .parse()
                        .map_err(|e| anyhow::anyhow!("unknown provider '{name}': {e}"))?;
                    vec![(p, consilium::auth::probe_auth(p, &store).await)]
                }
                None => consilium::auth::auth_report(&store).await,
            };
            let ready = report
                .iter()
                .filter(|(_, s)| matches!(s, consilium::auth::ProviderAuth::Ready))
                .count();
            println!("── provider auth ──");
            for (p, status) in &report {
                let mark = if matches!(status, consilium::auth::ProviderAuth::Ready) {
                    "✓"
                } else {
                    "✗"
                };
                println!("  {mark} {}", consilium::auth::guidance(*p, status));
            }
            println!("{ready}/{} providers ready", report.len());
        }
        Command::Models { write } => {
            use consilium::event::Provider;
            use consilium::models::{self, TopModel};

            let files = consilium::config_files::ConfigFiles::locate(&std::env::current_dir()?);
            let config = files.load()?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;

            println!("── best model each provider can run on this account ──");
            let resolved = models::resolve_top_models(&store).await;
            for (p, top) in &resolved {
                match top {
                    TopModel::Live(m) => println!("  ✓ {:8} {m}", p.as_str()),
                    TopModel::NoLiveModel => {
                        println!(
                            "  ✗ {:8} no model answered — check `consilium auth`",
                            p.as_str()
                        )
                    }
                    TopModel::CliMissing => {
                        println!("  ✗ {:8} CLI not installed", p.as_str())
                    }
                }
            }
            let chosen: Vec<(Provider, String)> = models::live_models(&resolved);

            // Retired models whose provider did not answer cannot be moved —
            // say so instead of silently leaving them.
            for s in models::stale_models(&config) {
                if !chosen.iter().any(|(p, _)| *p == s.provider) {
                    println!(
                        "\n  ! {}/{} is retired, but {} did not answer — nothing to switch it to yet.",
                        s.provider.as_str(),
                        s.current,
                        s.provider.as_str()
                    );
                }
            }

            let changes = models::planned_changes(&config, &chosen);
            if changes.is_empty() {
                println!("\nyour configuration is up to date.");
            } else {
                println!("\nmodels to switch (retired, or not available on this account):");
                for c in &changes {
                    println!("  • {}/{} → {}", c.provider.as_str(), c.from, c.to);
                }
                if write {
                    // Each file is fixed where it is. With no file at all, the
                    // upgraded built-in lineup becomes this project's config.
                    let updated = if files.existing().is_empty() {
                        let target =
                            std::path::PathBuf::from(consilium::config_files::PROJECT_CONFIG_FILE);
                        let upgraded = models::upgrade_config(&config, &chosen);
                        std::fs::write(&target, upgraded.to_pretty_json()?)?;
                        vec![target]
                    } else {
                        models::upgrade_files(&files, &chosen)?
                    };
                    println!();
                    for file in updated {
                        println!("✓ updated {}.", file.display());
                    }
                } else {
                    println!("\nrun `consilium models --write` to apply.");
                }
            }
        }
        Command::Run {
            provider,
            model,
            prompt,
        } => {
            use consilium::adapters::{
                claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter,
                grok::GrokAdapter, Adapter, RunRequest,
            };
            use consilium::event::{AgentEvent, Provider};
            use std::sync::Arc;

            let provider: Provider = provider.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let adapter: Arc<dyn Adapter> = match provider {
                Provider::Claude => Arc::new(ClaudeAdapter),
                Provider::Codex => Arc::new(CodexAdapter),
                Provider::Gemini => Arc::new(GeminiAdapter),
                Provider::Grok => Arc::new(GrokAdapter),
            };
            let req = RunRequest {
                prompt,
                model,
                cwd: std::env::current_dir()?,
                // Direct `run` may execute tools in cwd — keep provider safeguards armed.
                advisory: false,
                write: false,
            };
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;

            let mut handle = consilium::sessions::spawn(adapter, req)?;
            println!("session: {}", handle.id);
            let mut failed = false;
            while let Some(ev) = handle.events.recv().await {
                match &ev {
                    AgentEvent::Usage {
                        input_tokens,
                        output_tokens,
                    } => {
                        // Usage is recorded regardless of subsequent failure — real tokens were
                        // spent. M2 can associate the session id with a failed outcome separately.
                        store.record(provider, *input_tokens, *output_tokens)?;
                        println!("[usage] in={input_tokens} out={output_tokens}");
                    }
                    AgentEvent::Message { text } => println!("[message] {text}"),
                    AgentEvent::ToolCall { name, .. } => println!("[tool] {name}"),
                    AgentEvent::Completed { .. } => println!("[completed]"),
                    AgentEvent::Failed { error } => {
                        failed = true;
                        println!("[failed] {error}")
                    }
                    other => println!("[event] {other:?}"),
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        Command::Quota => {
            use consilium::event::Provider;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let since = consilium::quota::unix_now() - 5 * 3600;
            println!("usage in the last 5h window:");
            let mut any_estimated = false;
            for p in [
                Provider::Claude,
                Provider::Codex,
                Provider::Gemini,
                Provider::Grok,
            ] {
                let (input, output) = store.totals_since(p, since)?;
                let (est_in, est_out) = store.estimated_totals_since(p, since)?;
                // Tokens with no CLI usage report (e.g. Gemini via agy, or — until
                // real fixtures are recorded — the beta Grok CLI) are heuristic
                // estimates, not measured — flag them so the accounting stays
                // honest (the headline feature).
                let estimated = est_in + est_out > 0;
                any_estimated |= estimated;
                let marker = if estimated { "  (est.)" } else { "" };
                println!("  {:8} in={input:>8} out={output:>8}{marker}", p.as_str());
            }
            if any_estimated {
                println!(
                    "\n  (est.) = estimated (the CLI reports no usage, e.g. Gemini via agy, or the beta Grok CLI) — not measured"
                );
            }
        }
        Command::Council { question, timeout } => {
            use consilium::orchestrator::resilience::{ModelHealth, RetryConfig};
            use consilium::orchestrator::{council, roles, transcript::TranscriptStore};

            let config = consilium::config_files::load_effective()?;
            print_staleness_hint(&config);
            let chairman_ladder = roles::resolve_ladder(&config.roles.chairman);
            let members = roles::resolve_workers(&config.roles.workers);

            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let health = ModelHealth::with_retry(RetryConfig::prod());
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);
            let outcome = council::run_council(
                &question,
                members,
                chairman_ladder,
                &store,
                std::env::current_dir()?,
                std::time::Duration::from_secs(timeout),
                &health,
            )
            .await?;
            let path = transcripts.save("council", &outcome.transcript)?;
            println!("\n════ COUNCIL SYNTHESIS ════\n");
            println!("{}", outcome.synthesis);
            if !outcome.failed_members.is_empty() {
                println!("\n(members failed: {})", outcome.failed_members.join(", "));
            }
            // Print any model fallbacks that occurred during the council run.
            let fallbacks = outcome.transcript["fallbacks"].as_array();
            if let Some(fbs) = fallbacks {
                if !fbs.is_empty() {
                    println!("\n(model fallbacks: {})", fbs.len());
                    for fb in fbs {
                        println!(
                            "  ↳ {} → {}",
                            fb["from"].as_str().unwrap_or("?"),
                            fb["to"].as_str().unwrap_or("?")
                        );
                    }
                }
            }
            println!("\ntranscript: {}", path.display());
        }
        Command::Review {
            staged,
            diff_file,
            author_model,
            no_cross_examine,
            timeout,
        } => {
            use consilium::family::ModelFamily;
            use consilium::orchestrator::cross_examine::{
                any_blocks, examine_review, findings_record, Parties,
            };
            use consilium::orchestrator::resilience::{ModelHealth, RetryConfig};
            use consilium::orchestrator::{review, roles, transcript::TranscriptStore};

            let cwd = std::env::current_dir()?;
            let diff = match diff_file {
                Some(path) => std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?,
                None => {
                    let mut cmd = std::process::Command::new("git");
                    cmd.arg("diff").current_dir(&cwd);
                    if staged {
                        cmd.arg("--staged");
                    }
                    let out = cmd.output()?;
                    if !out.status.success() {
                        anyhow::bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr));
                    }
                    String::from_utf8_lossy(&out.stdout).into_owned()
                }
            };
            if diff.trim().is_empty() {
                anyhow::bail!("nothing to review: the diff is empty");
            }
            // Guessing an author that cannot be read would let the author's
            // own family pass for an independent examiner.
            let author = match author_model.as_deref() {
                None => ModelFamily::Anthropic,
                Some(name) => ModelFamily::of_author(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "--author-model {name}: cannot tell which model family that is — \
                         write it as provider/model, e.g. codex/gpt-6-luna"
                    )
                })?,
            };

            let config = consilium::config_files::load_effective()?;
            print_staleness_hint(&config);
            // The reviewer's full ladder, like council and conduct: a retired
            // or plan-gated primary falls through to the next rung instead of
            // failing the review outright.
            let ladder = roles::resolve_ladder(&config.roles.reviewer);
            let health = ModelHealth::with_retry(RetryConfig::prod());
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);

            let timeout = std::time::Duration::from_secs(timeout);
            let (result, fallbacks) = review::run_review_ladder(
                &diff,
                None,
                &ladder,
                &health,
                &store,
                cwd.clone(),
                timeout,
            )
            .await?;
            if let Some(model) = &result.model_used {
                println!("reviewed by {model}");
            }
            for fb in &fallbacks {
                println!("  ↳ {} → {} ({})", fb.from, fb.to, fb.reason);
            }
            let Some(verdict) = &result.verdict else {
                let path = transcripts.save("review", &result.transcript)?;
                println!("(reviewer output was not structured JSON — raw review below)\n");
                println!("{}", result.raw_review);
                println!("\ntranscript: {}", path.display());
                // An unparseable security review must fail CLOSED: CI can
                // distinguish "critical found" (2) from "review unusable" (3).
                std::process::exit(3);
            };

            // Neither the reviewer nor the author gets the last word on a
            // finding worth acting on: a model of another family rules on it.
            let workers = roles::resolve_workers(&config.roles.workers);
            let examined = if no_cross_examine {
                None
            } else {
                let parties = Parties {
                    reviewer_ladder: &ladder,
                    workers: &workers,
                    reviewer_model: result.model_used.as_deref(),
                    author,
                    independent_only: false,
                    task: None,
                };
                examine_review(
                    &diff,
                    &verdict.findings,
                    &parties,
                    &health,
                    &store,
                    cwd,
                    timeout,
                )
                .await
                .unwrap_or_else(|e| {
                    println!("findings not cross-examined: {e}");
                    None
                })
            };
            if let Some(examined) = &examined {
                println!(
                    "cross-examined by {} ({})",
                    examined.examiner,
                    examined.independence.as_str()
                );
            }

            if verdict.findings.is_empty() {
                println!("✓ clean — no findings");
            }
            for (index, f) in verdict.findings.iter().enumerate() {
                println!("[{:?}] {} — {}", f.severity, f.file, f.description);
                if let Some((ruled, _)) = examined.as_ref().and_then(|e| e.on(index)) {
                    println!("    ↳ {}: {}", ruled.ruling.as_str(), ruled.reason);
                }
            }

            let record = serde_json::json!({
                "author_model": author_model,
                "findings": findings_record(&verdict.findings, examined.as_ref()),
                "examined_by": examined.as_ref().map(|e| e.examiner.as_str()),
                "examiner_independence": examined.as_ref().map(|e| e.independence.as_str()),
            });
            let path = transcripts.save("review", &merged(&result.transcript, &record))?;
            println!("\ntranscript: {}", path.display());
            // Exit 2 while a critical finding stands. Only a refutation by a
            // model independent of the reviewer and of a NAMED author lifts it.
            let lifting = examined.as_ref().filter(|_| author_model.is_some());
            if examined.is_some() && lifting.is_none() {
                println!(
                    "(no --author-model: the rulings are shown, but none lifts a critical finding)"
                );
            }
            if any_blocks(&verdict.findings, lifting) {
                std::process::exit(2);
            }
        }
        Command::Standings { days, json } => {
            use consilium::orchestrator::transcript::TranscriptStore;
            const DAY_SECS: u64 = 24 * 60 * 60;
            let within =
                (days > 0).then(|| std::time::Duration::from_secs(days.saturating_mul(DAY_SECS)));
            let runs = consilium::standings::load(&TranscriptStore::default_base()?, within);
            let standings = consilium::standings::tally(&runs);
            if json {
                println!("{}", serde_json::to_string_pretty(&standings)?);
            } else {
                print!("{}", consilium::standings::render(&standings));
            }
        }
        Command::Conduct {
            task,
            context,
            no_preflight,
            timeout,
        } => {
            use consilium::orchestrator::conduct::{run_conduct, ConductDeps, RoleHandle};
            use consilium::orchestrator::resilience::{ModelHealth, RetryConfig};
            use consilium::orchestrator::{roles, transcript::TranscriptStore};

            let config = consilium::config_files::load_effective()?;
            print_staleness_hint(&config);
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            if !no_preflight {
                let report = consilium::doctor::preflight(&config, &store).await;
                consilium::doctor::print_preflight(&report);
                let conductor_candidates = config.roles.conductor.ladder();
                if !conductor_candidates
                    .iter()
                    .any(|candidate| report.is_alive(candidate.provider, &candidate.model))
                {
                    eprintln!(
                        "preflight: the conductor has no reachable model — re-authenticate the relevant CLI or fix consilium.config.json (run `consilium doctor --models` for detail) — or run `consilium init` to set up your council from scratch."
                    );
                    std::process::exit(1);
                }
                if !report.all_ok() {
                    eprintln!(
                        "preflight: {} model(s) unreachable — those roles will fail over or degrade; continuing.",
                        report.dead().len()
                    );
                }
            }
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);
            let cwd = std::env::current_dir()?;

            let workers = roles::resolve_workers(&config.roles.workers);

            let health = ModelHealth::with_retry(RetryConfig::prod());
            let deps = ConductDeps {
                conductor: RoleHandle {
                    ladder: roles::resolve_ladder(&config.roles.conductor),
                },
                workers,
                supervisor: Some(RoleHandle {
                    ladder: roles::resolve_ladder(&config.roles.supervisor),
                }),
                reviewer: Some(RoleHandle {
                    ladder: roles::resolve_ladder(&config.roles.reviewer),
                }),
                arbiter: Some(RoleHandle {
                    ladder: roles::resolve_ladder(&config.roles.chairman),
                }),
                verify: config.verify.clone(),
                memory: config.conductor_memory.clone().unwrap_or_default(),
                cross_family_review: config.cross_family_review,
                cross_examine: config.cross_examine,
                max_replans: config.max_replans,
                budget: config.budget_secs.map(std::time::Duration::from_secs),
            };

            let ctx = context.as_deref().unwrap_or("");
            let outcome = run_conduct(
                &task,
                ctx,
                deps,
                &store,
                cwd,
                std::time::Duration::from_secs(timeout),
                &health,
            )
            .await?;
            let path = transcripts.save("conduct", &outcome.transcript)?;

            println!("completed subtasks: {:?}", outcome.completed);
            if let Some(ref reason) = outcome.halted {
                println!("halted: {reason}");
            }
            if let Some(ref reason) = outcome.failed {
                println!("failed: {reason}");
            }
            // Print any model fallbacks that occurred during the conduct run.
            let fallbacks = outcome.transcript["fallbacks"].as_array();
            if let Some(fbs) = fallbacks {
                if !fbs.is_empty() {
                    println!("\n(model fallbacks: {})", fbs.len());
                    for fb in fbs {
                        println!(
                            "  ↳ {} → {} ({})",
                            fb["from"].as_str().unwrap_or("?"),
                            fb["to"].as_str().unwrap_or("?"),
                            fb["reason"].as_str().unwrap_or("?"),
                        );
                    }
                }
            }
            println!("transcript: {}", path.display());

            let success = outcome.halted.is_none()
                && outcome.failed.is_none()
                && !outcome.completed.is_empty();
            if !success {
                std::process::exit(1);
            }
        }
        Command::Auto {
            task,
            check,
            no_preflight,
            timeout,
        } => {
            use consilium::orchestrator::auto::{run_auto, AutoDeps};
            use consilium::orchestrator::conduct::{ConductDeps, RoleHandle};
            use consilium::orchestrator::{roles, transcript::TranscriptStore};

            let config = consilium::config_files::load_effective()?;
            print_staleness_hint(&config);
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            if !no_preflight {
                let report = consilium::doctor::preflight(&config, &store).await;
                consilium::doctor::print_preflight(&report);
                let conductor_candidates = config.roles.conductor.ladder();
                if !conductor_candidates
                    .iter()
                    .any(|candidate| report.is_alive(candidate.provider, &candidate.model))
                {
                    eprintln!(
                        "preflight: the conductor has no reachable model — re-authenticate the relevant CLI or fix consilium.config.json (run `consilium doctor --models` for detail) — or run `consilium init` to set up your council from scratch."
                    );
                    std::process::exit(1);
                }
                if !report.all_ok() {
                    eprintln!(
                        "preflight: {} model(s) unreachable — those roles will fail over or degrade; continuing.",
                        report.dead().len()
                    );
                }
            }
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);
            let cwd = std::env::current_dir()?;

            let workers = roles::resolve_workers(&config.roles.workers);

            let council_members = roles::resolve_workers(&config.roles.workers);

            let deps = AutoDeps {
                conduct: ConductDeps {
                    conductor: RoleHandle {
                        ladder: roles::resolve_ladder(&config.roles.conductor),
                    },
                    workers,
                    supervisor: Some(RoleHandle {
                        ladder: roles::resolve_ladder(&config.roles.supervisor),
                    }),
                    reviewer: Some(RoleHandle {
                        ladder: roles::resolve_ladder(&config.roles.reviewer),
                    }),
                    arbiter: Some(RoleHandle {
                        ladder: roles::resolve_ladder(&config.roles.chairman),
                    }),
                    verify: config.verify.clone(),
                    memory: config.conductor_memory.clone().unwrap_or_default(),
                    cross_family_review: config.cross_family_review,
                    cross_examine: config.cross_examine,
                    max_replans: config.max_replans,
                    budget: config.budget_secs.map(std::time::Duration::from_secs),
                },
                council_members,
                chairman: RoleHandle {
                    ladder: roles::resolve_ladder(&config.roles.chairman),
                },
            };

            let outcome = run_auto(
                &task,
                deps,
                &store,
                cwd,
                std::time::Duration::from_secs(timeout),
                check.as_deref(),
            )
            .await?;
            let path = transcripts.save("auto", &outcome.transcript)?;

            println!(
                "triage: {}",
                if outcome.triage_trivial {
                    "trivial"
                } else {
                    "standard"
                }
            );
            if let Some(ref synthesis) = outcome.council_synthesis {
                println!(
                    "council synthesis: {}",
                    synthesis.chars().take(120).collect::<String>()
                );
            }
            println!("completed subtasks: {:?}", outcome.conduct.completed);
            if let Some(ref reason) = outcome.conduct.halted {
                println!("halted: {reason}");
            }
            if let Some(ref reason) = outcome.conduct.failed {
                println!("failed: {reason}");
            }
            if let Some((passed, ref output)) = outcome.check {
                println!("check: {}", if passed { "passed" } else { "FAILED" });
                if !output.is_empty() {
                    println!("check output:\n{output}");
                }
            }
            // Print any model fallbacks that occurred during the auto run.
            let fallbacks = outcome.transcript["fallbacks"].as_array();
            if let Some(fbs) = fallbacks {
                if !fbs.is_empty() {
                    println!("\n(model fallbacks: {})", fbs.len());
                    for fb in fbs {
                        println!(
                            "  ↳ {} → {} ({})",
                            fb["from"].as_str().unwrap_or("?"),
                            fb["to"].as_str().unwrap_or("?"),
                            fb["reason"].as_str().unwrap_or("?"),
                        );
                    }
                }
            }
            println!("transcript: {}", path.display());

            let success = outcome.conduct.halted.is_none()
                && outcome.conduct.failed.is_none()
                && !outcome.conduct.completed.is_empty()
                && outcome.check.as_ref().is_none_or(|(passed, _)| *passed);
            if !success {
                std::process::exit(1);
            }
        }
        Command::Init { force, yes, global } => {
            use std::io::IsTerminal;
            let target = if global {
                let file = consilium::config_files::user_config_path().ok_or_else(|| {
                    anyhow::anyhow!("$HOME is not set; cannot locate ~/.consilium")
                })?;
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                file
            } else {
                std::env::current_dir()?.join(consilium::config_files::PROJECT_CONFIG_FILE)
            };
            // Non-interactive when --yes, or when stdin isn't a TTY (CI/pipes) —
            // never hang a wizard waiting on input that won't come.
            if yes || !std::io::stdin().is_terminal() {
                if target.exists() && !force {
                    eprintln!(
                        "{} already exists; use --force to overwrite",
                        target.display()
                    );
                    std::process::exit(1);
                }
                let roles = consilium::recommend::recommend_roles(&consilium::catalog::catalog())?;
                let cfg = consilium::wizard::build_config(roles);
                std::fs::write(&target, cfg.to_pretty_json()?)?;
                let n_roles = 2 + cfg.roles.workers.len() + 1 + 1;
                println!(
                    "wrote {} ({n_roles} roles; edit model ladders as needed)",
                    target.display()
                );
                println!("Next: `consilium auth` checks the CLIs are signed in; `consilium models --write` then switches this lineup to the models your plans include.");
                println!("Then run your first task:  consilium conduct \"add a hello() function with a test\"");
            } else {
                let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
                consilium::wizard::run_init_wizard(&store, &target, force).await?;
            }
        }
        Command::Mcp => {
            // The MCP server owns stdout for JSON-RPC; logs already go to stderr
            // (see the subscriber setup). Load config + the shared quota store
            // and serve over stdio until the client disconnects.
            let files = consilium::config_files::ConfigFiles::locate(&std::env::current_dir()?);
            let config = files.load()?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            consilium::mcp::serve_stdio(config, store, files.describe()).await?;
        }
        Command::Serve { addr, timeout } => {
            let files = consilium::config_files::ConfigFiles::locate(&std::env::current_dir()?);
            let config = files.load()?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let socket_addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid --addr '{addr}': {e}"))?;
            consilium::server::serve(
                socket_addr,
                config,
                store,
                std::time::Duration::from_secs(timeout),
                files.describe(),
            )
            .await?;
        }
        Command::Eval {
            suite,
            approaches,
            trials,
            out,
            task,
            timeout,
            spend_quota,
        } => {
            use consilium::config::VerifyConfig;
            use consilium::orchestrator::conduct::{ConductDeps, RoleHandle};
            use consilium::orchestrator::eval::{self, EvalDeps};
            use consilium::orchestrator::resilience::Rung;
            use consilium::orchestrator::roles;

            let approaches = eval::parse_approaches(&approaches)?;
            let tasks = eval::load_suite(&suite, task.as_deref())?;
            if tasks.is_empty() {
                anyhow::bail!("no tasks found under {}", suite.display());
            }

            // Default: dry-run. Never call a model (or require config) without --spend-quota.
            if !spend_quota {
                print!("{}", eval::dry_run_plan(&tasks, &approaches, trials));
                return Ok(());
            }

            let config = consilium::config_files::load_effective()?;
            struct ConfigEvalDeps {
                config: consilium::config::Config,
            }
            impl EvalDeps for ConfigEvalDeps {
                fn solo_ladder(&self) -> Vec<Rung> {
                    // Baseline = the strongest single model alone (the conductor's
                    // ladder, e.g. Claude) — so conduct's win is measured against
                    // "just use the smart model by itself", matching the real-world
                    // claude-solo comparison.
                    roles::resolve_ladder(&self.config.roles.conductor)
                }
                fn conduct_deps(
                    &self,
                    verify: Option<VerifyConfig>,
                    cross_family: bool,
                ) -> ConductDeps {
                    let workers = roles::resolve_workers(&self.config.roles.workers);
                    ConductDeps {
                        conductor: RoleHandle {
                            ladder: roles::resolve_ladder(&self.config.roles.conductor),
                        },
                        workers,
                        supervisor: Some(RoleHandle {
                            ladder: roles::resolve_ladder(&self.config.roles.supervisor),
                        }),
                        reviewer: Some(RoleHandle {
                            ladder: roles::resolve_ladder(&self.config.roles.reviewer),
                        }),
                        arbiter: Some(RoleHandle {
                            ladder: roles::resolve_ladder(&self.config.roles.chairman),
                        }),
                        verify,
                        memory: self.config.conductor_memory.clone().unwrap_or_default(),
                        cross_family_review: cross_family,
                        cross_examine: self.config.cross_examine,
                        max_replans: 0,
                        budget: None,
                    }
                }
            }

            let deps = ConfigEvalDeps { config };
            let out_path = out.unwrap_or_else(|| {
                std::path::PathBuf::from("eval/results")
                    .join(format!("{}-results.json", consilium::quota::unix_now()))
            });
            eprintln!("results (saved after every run): {}", out_path.display());
            // Saved after every trial, so a run that is stopped or dies
            // part-way keeps what it has already measured and paid for.
            let mut save_progress = |partial: &eval::SuiteReport| {
                if let Err(e) = eval::save_report(&out_path, partial) {
                    eprintln!("warning: could not save partial results: {e:#}");
                }
            };
            // Recorded before the first model call: when this run started and
            // which commit of the suite it measured.
            let meta = eval::ReportMeta::capture(&suite);
            let report = eval::run_suite(
                &tasks,
                &approaches,
                trials,
                &deps,
                std::time::Duration::from_secs(timeout),
                meta,
                &mut save_progress,
            )
            .await?;

            println!("{}", eval::markdown_report(&report));
            eval::save_report(&out_path, &report)?;
            println!("results: {}", out_path.display());
        }
    }
    Ok(())
}
