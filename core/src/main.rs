use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "consilium",
    version,
    about = "Multi-agent orchestrator for subscription CLI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
        /// Hard per-session timeout in seconds
        #[arg(long, default_value_t = 900)]
        timeout: u64,
    },
    /// Write a starter consilium.config.json in the current directory
    Init {
        /// Overwrite an existing consilium.config.json
        #[arg(long)]
        force: bool,
    },
}

fn quota_db_path() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("$HOME is not set; cannot locate ~/.consilium/usage.db"))?;
    Ok(std::path::PathBuf::from(home)
        .join(".consilium")
        .join("usage.db"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.command {
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
                println!("  gemini: npm install -g @google/gemini-cli");
                println!("  claude: see https://code.claude.com");
                std::process::exit(1);
            }

            if models {
                use consilium::adapters::{
                    claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter,
                };
                use consilium::doctor::{collect_distinct_model_pairs, probe_model};
                use consilium::event::Provider;
                use std::sync::Arc;

                let config = consilium::config::Config::load(Some(std::path::Path::new(
                    "consilium.config.json",
                )))?;
                let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
                let pairs = collect_distinct_model_pairs(&config);

                println!("\n── Model availability ──");
                let mut any_failed = false;
                for candidate in pairs {
                    let adapter: Arc<dyn consilium::adapters::Adapter> = match candidate.provider {
                        Provider::Claude => Arc::new(ClaudeAdapter),
                        Provider::Codex => Arc::new(CodexAdapter),
                        Provider::Gemini => Arc::new(GeminiAdapter),
                    };
                    let probe = probe_model(adapter, &candidate.model, &store).await;
                    if probe.ok {
                        println!("  ✓ {}/{}", candidate.provider.as_str(), candidate.model);
                    } else {
                        any_failed = true;
                        println!(
                            "  ✗ {}/{} — {}",
                            candidate.provider.as_str(),
                            candidate.model,
                            probe.detail
                        );
                    }
                }
                if any_failed {
                    std::process::exit(1);
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
            for p in [Provider::Claude, Provider::Codex, Provider::Gemini] {
                let (input, output) = store.totals_since(p, since)?;
                println!("  {:8} in={input:>8} out={output:>8}", p.as_str());
            }
        }
        Command::Council { question, timeout } => {
            use consilium::orchestrator::council::CouncilMember;
            use consilium::orchestrator::resilience::ModelHealth;
            use consilium::orchestrator::{council, roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
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
            let health = ModelHealth::new();
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
            timeout,
        } => {
            use consilium::orchestrator::conduct::{run_conduct, ConductDeps, RoleHandle};
            use consilium::orchestrator::council::CouncilMember;
            use consilium::orchestrator::resilience::ModelHealth;
            use consilium::orchestrator::{roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
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

            let health = ModelHealth::new();
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
            timeout,
        } => {
            use consilium::orchestrator::auto::{run_auto, AutoDeps};
            use consilium::orchestrator::conduct::{ConductDeps, RoleHandle};
            use consilium::orchestrator::council::CouncilMember;
            use consilium::orchestrator::{roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
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
        Command::Init { force } => {
            let target = std::env::current_dir()?.join("consilium.config.json");
            if target.exists() && !force {
                eprintln!("consilium.config.json already exists; use --force to overwrite");
                std::process::exit(1);
            }
            let cfg = consilium::config::Config::default();
            let json = cfg.to_pretty_json()?;
            // Count roles: conductor + chairman + workers + reviewer + supervisor
            let n_roles = 2 + cfg.roles.workers.len() + 1 + 1;
            std::fs::write(&target, &json)?;
            println!("wrote consilium.config.json ({n_roles} roles; edit model ladders as needed)");
        }
    }
    Ok(())
}
