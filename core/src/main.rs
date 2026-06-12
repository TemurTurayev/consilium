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
    Doctor,
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
        Command::Doctor => {
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
            };
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;

            let mut handle = consilium::sessions::spawn(adapter, req)?;
            println!("session: {}", handle.id);
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
                    AgentEvent::Failed { error } => println!("[failed] {error}"),
                    other => println!("[event] {other:?}"),
                }
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
            use consilium::orchestrator::{council, roles, transcript::TranscriptStore};

            let config = consilium::config::Config::load(Some(std::path::Path::new(
                "consilium.config.json",
            )))?;
            let chairman_model = Some(config.roles.chairman.model.clone());
            let chairman_adapter = roles::adapter_for(&config.roles.chairman);
            let members: Vec<CouncilMember> = config
                .roles
                .workers
                .iter()
                .map(|role| CouncilMember {
                    label: format!("{}-{}", role.provider.as_str(), role.model),
                    adapter: roles::adapter_for(role),
                    model: Some(role.model.clone()),
                })
                .collect();

            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);
            let outcome = council::run_council(
                &question,
                members,
                chairman_adapter,
                chairman_model,
                &store,
                std::env::current_dir()?,
                std::time::Duration::from_secs(timeout),
            )
            .await?;
            let path = transcripts.save("council", &outcome.transcript)?;
            println!("\n════ COUNCIL SYNTHESIS ════\n");
            println!("{}", outcome.synthesis);
            if !outcome.failed_members.is_empty() {
                println!("\n(members failed: {})", outcome.failed_members.join(", "));
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
    }
    Ok(())
}
