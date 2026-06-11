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
}

fn quota_db_path() -> anyhow::Result<std::path::PathBuf> {
    let home = std::env::var("HOME")?;
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
    }
    Ok(())
}
