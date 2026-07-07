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
        /// Probe just one provider (claude|codex|gemini) instead of all.
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
        #[arg(long, default_value_t = 600)]
        timeout: u64,
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
    },
    /// Check each provider's current top model (probes ~1 token per provider)
    /// and, with --write, update consilium.config.json so superseded models
    /// adopt the latest. Run it after a provider ships a newer model.
    Models {
        /// Rewrite consilium.config.json to adopt each provider's top live model.
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
            "ℹ superseded model(s) in config: {} — run `consilium models --write` to adopt the latest.",
            names.join(", ")
        );
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
                let config = consilium::config::Config::load(Some(std::path::Path::new(
                    "consilium.config.json",
                )))?;
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

            let config_path = std::path::Path::new("consilium.config.json");
            let config = consilium::config::Config::load(Some(config_path))?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;

            println!("── available top models ──");
            let resolved = models::resolve_top_models(&store).await;
            let mut chosen: Vec<(Provider, String)> = Vec::new();
            for (p, top) in &resolved {
                match top {
                    TopModel::Live(m) => {
                        println!("  ✓ {:8} {m}", p.as_str());
                        chosen.push((*p, m.clone()));
                    }
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

            let stale = models::stale_models(&config);
            if stale.is_empty() {
                println!("\nconsilium.config.json is up to date.");
            } else {
                println!("\nsuperseded models in consilium.config.json:");
                for s in &stale {
                    // Show what `--write` would actually adopt: the live-probed top
                    // (`chosen`) when available, falling back to the catalog top
                    // only when the provider has no live model. Keeps the preview
                    // honest when an account is gated out of the catalog's #1.
                    let live = chosen
                        .iter()
                        .find(|(p, _)| *p == s.provider)
                        .map(|(_, m)| m.as_str());
                    match live.or(s.suggested.as_deref()) {
                        Some(target) => {
                            println!("  • {}/{} → {target}", s.provider.as_str(), s.current)
                        }
                        None => {
                            println!(
                                "  • {}/{} (no catalog replacement)",
                                s.provider.as_str(),
                                s.current
                            )
                        }
                    }
                }
                if write {
                    let upgraded = models::upgrade_config(&config, &chosen);
                    if upgraded == config {
                        println!(
                            "\nnothing written — no live replacement available for the superseded provider(s)."
                        );
                    } else {
                        std::fs::write(config_path, upgraded.to_pretty_json()?)?;
                        println!(
                            "\n✓ updated {} to the latest models.",
                            config_path.display()
                        );
                    }
                } else {
                    println!("\nrun `consilium models --write` to adopt them.");
                }
            }
        }
        Command::Run {
            provider,
            model,
            prompt,
        } => {
            use consilium::adapters::{
                claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter, Adapter,
                RunRequest,
            };
            use consilium::event::{AgentEvent, Provider};
            use std::sync::Arc;

            let provider: Provider = provider.parse().map_err(|e: String| anyhow::anyhow!(e))?;
            let adapter: Arc<dyn Adapter> = match provider {
                Provider::Claude => Arc::new(ClaudeAdapter),
                Provider::Codex => Arc::new(CodexAdapter),
                Provider::Gemini => Arc::new(GeminiAdapter),
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
            for p in [Provider::Claude, Provider::Codex, Provider::Gemini] {
                let (input, output) = store.totals_since(p, since)?;
                let (est_in, est_out) = store.estimated_totals_since(p, since)?;
                // Tokens with no CLI usage report (e.g. Gemini via agy) are
                // heuristic estimates, not measured — flag them so the accounting
                // stays honest (the headline feature).
                let estimated = est_in + est_out > 0;
                any_estimated |= estimated;
                let marker = if estimated { "  (est.)" } else { "" };
                println!("  {:8} in={input:>8} out={output:>8}{marker}", p.as_str());
            }
            if any_estimated {
                println!(
                    "\n  (est.) = estimated (the CLI reports no usage, e.g. Gemini via agy) — not measured"
                );
            }
        }
        Command::Council { question, timeout } => {
            use consilium::orchestrator::council::CouncilMember;
            use consilium::orchestrator::resilience::{ModelHealth, RetryConfig};
            use consilium::orchestrator::{council, roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            print_staleness_hint(&config);
            let chairman_ladder = roles::resolve_ladder(&config.roles.chairman);
            let members: Vec<CouncilMember> = config
                .roles
                .workers
                .iter()
                .map(|role| CouncilMember {
                    label: format!("{}-{}", role.provider.as_str(), role.model),
                    ladder: roles::resolve_ladder(role),
                })
                .collect();

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
            timeout,
        } => {
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

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            let reviewer_role = &config.roles.reviewer;
            let reviewer = roles::adapter_for(reviewer_role);
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);

            let result = review::run_review(
                &diff,
                reviewer,
                Some(reviewer_role.model.clone()),
                &store,
                cwd,
                std::time::Duration::from_secs(timeout),
            )
            .await?;
            let path = transcripts.save("review", &result.transcript)?;

            match &result.verdict {
                Some(v) if v.findings.is_empty() => println!("✓ clean — no findings"),
                Some(v) => {
                    for f in &v.findings {
                        println!("[{:?}] {} — {}", f.severity, f.file, f.description);
                    }
                }
                None => {
                    println!("(reviewer output was not structured JSON — raw review below)\n");
                    println!("{}", result.raw_review);
                    println!("\ntranscript: {}", path.display());
                    // An unparseable security review must fail CLOSED: CI can
                    // distinguish "critical found" (2) from "review unusable" (3).
                    std::process::exit(3);
                }
            }
            println!("\ntranscript: {}", path.display());
            if result.verdict.as_ref().is_some_and(|v| v.has_critical()) {
                std::process::exit(2);
            }
        }
        Command::Conduct {
            task,
            context,
            no_preflight,
            timeout,
        } => {
            use consilium::orchestrator::conduct::{run_conduct, ConductDeps, RoleHandle};
            use consilium::orchestrator::council::CouncilMember;
            use consilium::orchestrator::resilience::{ModelHealth, RetryConfig};
            use consilium::orchestrator::{roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
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

            let workers: Vec<CouncilMember> = config
                .roles
                .workers
                .iter()
                .map(|role| CouncilMember {
                    label: format!("{}-{}", role.provider.as_str(), role.model),
                    ladder: roles::resolve_ladder(role),
                })
                .collect();

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
            use consilium::orchestrator::council::CouncilMember;
            use consilium::orchestrator::{roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
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

            let workers: Vec<CouncilMember> = config
                .roles
                .workers
                .iter()
                .map(|role| CouncilMember {
                    label: format!("{}-{}", role.provider.as_str(), role.model),
                    ladder: roles::resolve_ladder(role),
                })
                .collect();

            let council_members: Vec<CouncilMember> = config
                .roles
                .workers
                .iter()
                .map(|role| CouncilMember {
                    label: format!("{}-{}", role.provider.as_str(), role.model),
                    ladder: roles::resolve_ladder(role),
                })
                .collect();

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
        Command::Init { force, yes } => {
            use std::io::IsTerminal;
            let target = std::env::current_dir()?.join("consilium.config.json");
            // Non-interactive when --yes, or when stdin isn't a TTY (CI/pipes) —
            // never hang a wizard waiting on input that won't come.
            if yes || !std::io::stdin().is_terminal() {
                if target.exists() && !force {
                    eprintln!("consilium.config.json already exists; use --force to overwrite");
                    std::process::exit(1);
                }
                let roles = consilium::recommend::recommend_roles(&consilium::catalog::catalog())?;
                let cfg = consilium::wizard::build_config(roles);
                std::fs::write(&target, cfg.to_pretty_json()?)?;
                let n_roles = 2 + cfg.roles.workers.len() + 1 + 1;
                println!(
                    "wrote consilium.config.json ({n_roles} roles; edit model ladders as needed)"
                );
                println!("Next: authenticate providers with `consilium auth`, then verify with `consilium doctor --models`.");
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
            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            consilium::mcp::serve_stdio(config, store).await?;
        }
        Command::Serve { addr, timeout } => {
            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let socket_addr: std::net::SocketAddr = addr
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid --addr '{addr}': {e}"))?;
            consilium::server::serve(
                socket_addr,
                config,
                store,
                std::time::Duration::from_secs(timeout),
                Some("consilium.config.json".to_string()),
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
            use consilium::orchestrator::council::CouncilMember;
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

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
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
                    let workers = self
                        .config
                        .roles
                        .workers
                        .iter()
                        .map(|role| CouncilMember {
                            label: format!("{}-{}", role.provider.as_str(), role.model),
                            ladder: roles::resolve_ladder(role),
                        })
                        .collect();
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
                        max_replans: 0,
                        budget: None,
                    }
                }
            }

            let deps = ConfigEvalDeps { config };
            let report = eval::run_suite(
                &tasks,
                &approaches,
                trials,
                &deps,
                std::time::Duration::from_secs(timeout),
            )
            .await?;

            println!("{}", eval::markdown_report(&report));

            let out_path = out.unwrap_or_else(|| {
                std::path::PathBuf::from("eval/results")
                    .join(format!("{}-results.json", consilium::quota::unix_now()))
            });
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
            println!("results: {}", out_path.display());
        }
    }
    Ok(())
}
