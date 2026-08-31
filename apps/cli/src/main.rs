//! kano-proxy — the product's official CLI (docs/cli.md).

mod api;
mod commands;
mod protocol;
mod state;
mod tui;
mod tunnel;
mod update;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::state::{default_state_path, StateFile};

#[derive(Parser)]
#[command(
    name = "kano-proxy",
    version,
    about = "Expose local LLM servers as kano-proxy providers over the agent tunnel",
    propagate_version = true
)]
struct Cli {
    /// State file path (default: ~/.config/kano-proxy/state.json)
    #[arg(long, global = true)]
    state: Option<PathBuf>,

    /// Non-interactive mode: flags instead of the interactive screens
    #[arg(long, global = true)]
    no_tui: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign this device in (once per machine)
    Init {
        /// Server origin, e.g. https://proxy.example.com
        #[arg(long)]
        base_url: Option<String>,
        /// Name shown on the web UI's CLI page
        #[arg(long)]
        device_name: Option<String>,
        /// Second phase of --no-tui: the code shown on the authorize page
        #[arg(long)]
        auth_code: Option<String>,
    },
    /// Register one local endpoint as a CLI provider
    Add {
        #[arg(long)]
        slug: Option<String>,
        /// openai | anthropic
        #[arg(long)]
        format: Option<String>,
        /// Local target base URL (include /v1 for openai format)
        #[arg(long)]
        target: Option<String>,
        /// The local server's own API key, if it needs one
        #[arg(long)]
        target_key: Option<String>,
        /// Comma-separated expose whitelist (omitted = follow the local server)
        #[arg(long)]
        expose: Option<String>,
    },
    /// Unregister a provider (server + local state)
    Remove {
        slug: String,
        /// Skip the server call (e.g. already deleted in the web UI)
        #[arg(long)]
        local_only: bool,
    },
    /// This device's registered providers and their live state
    List,
    /// Run the tunnel: one connection per registered provider (foreground)
    Start {
        /// In-flight request cap per provider, 1-4 (a raise past 4 is refused)
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
    /// Device auth state plus per-provider connection state
    Status,
    /// Self-update from the latest GitHub Release (checksum-verified)
    Update,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let file = StateFile::new(cli.state.clone().unwrap_or_else(default_state_path));

    let result = match cli.command {
        Command::Init { base_url, device_name, auth_code } => {
            commands::cmd_init(
                &file,
                commands::InitArgs { no_tui: cli.no_tui, base_url, device_name, auth_code },
            )
            .await
        }
        Command::Add { slug, format, target, target_key, expose } => {
            commands::cmd_add(
                &file,
                commands::AddArgs { no_tui: cli.no_tui, slug, format, target, target_key, expose },
            )
            .await
        }
        Command::Remove { slug, local_only } => commands::cmd_remove(&file, &slug, local_only).await,
        Command::List => commands::cmd_list(&file).await,
        Command::Start { concurrency } => commands::cmd_start(file, concurrency).await,
        Command::Status => commands::cmd_status(&file).await,
        Command::Update => update::cmd_update().await,
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        // 1 = usage/config error; auth-rejected paths exit 2 before reaching here.
        std::process::exit(1);
    }
}
