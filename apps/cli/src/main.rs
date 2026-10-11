//! kano-proxy — the product's official CLI (docs/cli.md § CLI).
//!
//! Two jobs, two namespaces: `tunnel` exposes local LLM servers as providers
//! over the CLI tunnel; `agent` points coding agents on this machine at the
//! proxy. Device-level commands (`login`, `logout`, `status`, `update`) stay
//! top-level.

mod agents;
mod api;
mod commands;
mod protocol;
mod state;
mod tui;
mod tunnel;
mod update;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::agents::AgentKind;
use crate::state::{default_state_path, StateFile};

#[derive(Parser)]
#[command(
    name = "kano-proxy",
    version,
    about = "Tunnel local LLM servers in as kano-proxy providers, and point Claude Code or the Codex CLI at the proxy",
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
    /// Sign this device in (once per machine; signs in again when the server revoked this device)
    Login {
        /// Server origin, e.g. https://proxy.example.com
        #[arg(long)]
        base_url: Option<String>,
        /// Name shown on the web UI's Access page
        #[arg(long)]
        device_name: Option<String>,
        /// Second phase of --no-tui: the code shown on the authorize page
        #[arg(long)]
        auth_code: Option<String>,
    },
    /// Restore every agent set up here, revoke this device (and the keys it minted), forget the sign-in
    Logout {
        /// Overwrite agent config fields edited since setup (required with --no-tui when they drifted)
        #[arg(long)]
        force: bool,
        /// Also delete the model groups the agents were set up with
        #[arg(long)]
        remove_group: bool,
    },
    /// Device auth state, tunnel connection state and agent state
    Status,
    /// Self-update from the latest GitHub Release (checksum-verified)
    Update,
    /// Expose local LLM servers as providers over the CLI tunnel
    #[command(subcommand)]
    Tunnel(TunnelCommand),
    /// Point a coding agent on this machine at the proxy, or undo it
    #[command(subcommand)]
    Agent(AgentCommand),
}

#[derive(Subcommand)]
enum TunnelCommand {
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
        /// The local server's own API key, if it needs one. Prefer the
        /// KANO_PROXY_TARGET_KEY environment variable — argv leaks into shell
        /// history and the process list; the flag remains for controlled
        /// environments.
        #[arg(long, env = "KANO_PROXY_TARGET_KEY", hide_env_values = true)]
        target_key: Option<String>,
        /// Comma-separated expose whitelist (omitted = follow the local server)
        #[arg(long)]
        expose: Option<String>,
    },
    /// Serve an existing provider from this device (registered elsewhere)
    Attach {
        /// The provider's slug, as shown by `kano-proxy tunnel list`
        slug: String,
        /// Local target base URL on this machine (include /v1 for openai format)
        #[arg(long)]
        target: Option<String>,
        /// The local server's own API key, if it needs one (prefer the env var)
        #[arg(long, env = "KANO_PROXY_TARGET_KEY", hide_env_values = true)]
        target_key: Option<String>,
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
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Write the proxy into the agent's config: a model group with its real model names, a key for this device
    Setup {
        /// claude-code | codex
        agent: AgentKind,
        /// Print what would be written, minted and created; change nothing
        #[arg(long)]
        dry_run: bool,
        /// claude-code, --no-tui: the bare id the Opus slot resolves to
        #[arg(long)]
        opus: Option<String>,
        /// claude-code, --no-tui: the bare id the Sonnet slot resolves to
        #[arg(long)]
        sonnet: Option<String>,
        /// claude-code, --no-tui: the bare id the Haiku slot resolves to
        #[arg(long)]
        haiku: Option<String>,
        /// codex, --no-tui: the bare id Codex starts with
        #[arg(long)]
        model: Option<String>,
        /// codex, --no-tui: low | medium | high | xhigh (default high)
        #[arg(long)]
        effort: Option<String>,
        /// Fallback target(s) for the picked model(s), as provider/model ids from your catalog; repeatable
        #[arg(long)]
        fallback: Vec<String>,
    },
    /// Undo setup: fields back to before (default), or the whole file from the backup (--full); revoke the key
    Restore {
        /// claude-code | codex
        agent: AgentKind,
        /// Put the pre-setup backup back in place of the whole file (the user's later edits are lost)
        #[arg(long)]
        full: bool,
        /// Proceed without confirmation past edited fields or a --full replace (required with --no-tui)
        #[arg(long)]
        force: bool,
        /// With --full: the backup to use instead of the one recorded at setup
        #[arg(long)]
        backup: Option<PathBuf>,
        /// Also delete the model group (account-level — other devices may use it)
        #[arg(long)]
        remove_group: bool,
        /// Print the plan and change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Which agents this device set up, and whether their config still holds what was written
    List,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let file = StateFile::new(cli.state.clone().unwrap_or_else(default_state_path));

    let result = match cli.command {
        Command::Login { base_url, device_name, auth_code } => {
            commands::cmd_login(&file, commands::LoginArgs { no_tui: cli.no_tui, base_url, device_name, auth_code }).await
        }
        Command::Logout { force, remove_group } => commands::cmd_logout(&file, cli.no_tui, force, remove_group).await,
        Command::Status => commands::cmd_status(&file).await,
        Command::Update => update::cmd_update().await,
        Command::Tunnel(sub) => match sub {
            TunnelCommand::Add { slug, format, target, target_key, expose } => {
                commands::cmd_add(&file, commands::AddArgs { no_tui: cli.no_tui, slug, format, target, target_key, expose }).await
            }
            TunnelCommand::Attach { slug, target, target_key } => commands::cmd_attach(&file, &slug, target, target_key).await,
            TunnelCommand::Remove { slug, local_only } => commands::cmd_remove(&file, &slug, local_only).await,
            TunnelCommand::List => commands::cmd_list(&file).await,
            TunnelCommand::Start { concurrency } => commands::cmd_start(file, concurrency).await,
        },
        Command::Agent(sub) => match sub {
            AgentCommand::Setup { agent, dry_run, opus, sonnet, haiku, model, effort, fallback } => {
                agents::cmd_setup(
                    &file,
                    agent,
                    agents::SetupArgs { no_tui: cli.no_tui, dry_run, opus, sonnet, haiku, model, effort, fallback },
                )
                .await
            }
            AgentCommand::Restore { agent, full, force, backup, remove_group, dry_run } => {
                agents::cmd_restore(
                    &file,
                    agent,
                    agents::RestoreArgs { no_tui: cli.no_tui, dry_run, full, force, backup, remove_group },
                )
                .await
            }
            AgentCommand::List => agents::cmd_list(&file),
        },
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        // 1 = usage/config error; auth-rejected paths exit 2 before reaching here.
        std::process::exit(1);
    }
}
