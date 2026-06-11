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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => {
            let mut all_ok = true;
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
            println!("run: not implemented yet ({provider}, {model:?}, {prompt})");
        }
        Command::Quota => println!("quota: not implemented yet"),
    }
    Ok(())
}
