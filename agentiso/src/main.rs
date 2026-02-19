mod cli;
mod config;
mod dashboard;
mod guest;
mod mcp;
mod network;
mod storage;
mod vm;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::network::NetworkManager;
use crate::storage::StorageManager;
use crate::vm::{VmManager, VmManagerConfig};
use crate::workspace::WorkspaceManager;

#[derive(Parser)]
#[command(name = "agentiso", about = "QEMU microvm workspace manager for AI agents")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (stdio transport).
    Serve {
        /// Path to config file (TOML).
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
    /// Verify all prerequisites before running 'serve'. Exits 0 if all pass.
    Check {
        /// Path to config file (TOML).
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
    /// Show current workspace state from the state file (no daemon needed).
    Status {
        /// Path to config file (TOML).
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
    /// Launch the interactive TUI dashboard.
    Dashboard {
        /// Path to config file (TOML).
        #[arg(long, short)]
        config: Option<PathBuf>,
        /// Refresh interval in seconds (default: 2).
        #[arg(long, default_value = "2")]
        refresh: u64,
    },
    /// Show QEMU console and stderr logs for a workspace (no daemon needed).
    Logs {
        /// Workspace ID or short ID prefix (first 8 chars).
        workspace_id: String,
        /// Number of lines to show from the end of console.log (default: 50).
        #[arg(long, short = 'n', default_value = "50")]
        lines: usize,
        /// Path to config file (TOML).
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Dashboard { config: config_path, refresh } => {
            let config = cli::load_config(config_path.clone())?;
            dashboard::run(config, config_path, refresh)?;
        }
        Commands::Check { config: config_path } => {
            let config = cli::load_config(config_path)?;
            cli::run_check(&config)?;
        }
        Commands::Status { config: config_path } => {
            let config = cli::load_config(config_path)?;
            cli::run_status(&config)?;
        }
        Commands::Logs { workspace_id, lines, config: config_path } => {
            let config = cli::load_config(config_path)?;
            cli::run_logs(&config, &workspace_id, lines)?;
        }
        Commands::Serve { config: config_path } => {
            let config = match config_path {
                Some(path) => Config::load(&path)?,
                None => Config::default(),
            };

            // Acquire exclusive instance lock to prevent concurrent daemons
            // sharing the same state file, IP allocator, and nftables table.
            let lock_path = config
                .server
                .state_file
                .parent()
                .map(|p| p.join("agentiso.lock"))
                .unwrap_or_else(|| PathBuf::from("/var/lib/agentiso/agentiso.lock"));
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let lock_file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .open(&lock_path)
                .context("failed to open instance lock file")?;
            use std::os::unix::io::AsRawFd;
            let fd = lock_file.as_raw_fd();
            let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                anyhow::bail!(
                    "Another agentiso instance is already running.\n\
                     Two concurrent instances share the same state file, IP allocator, \
                     and nftables table — this would cause data corruption.\n\
                     Stop the other instance first, or check: agentiso status"
                );
            }
            // lock_file must stay alive for the entire serve duration (dropped at end of block)

            tracing::info!("agentiso starting with config: {:?}", config);

            // Build subsystem managers from config
            let pool_root = format!(
                "{}/{}",
                config.storage.zfs_pool, config.storage.dataset_prefix
            );
            let storage = StorageManager::new(pool_root);

            let network = NetworkManager::with_config(
                config.network.bridge_name.clone(),
                format!("{}/{}", config.network.gateway_ip, config.network.subnet_prefix),
                format!(
                    "{}/{}",
                    {
                        // Compute subnet address (zero out host bits)
                        let gw = config.network.gateway_ip;
                        let mask = !((1u32 << (32 - config.network.subnet_prefix)) - 1);
                        let net = u32::from(gw) & mask;
                        std::net::Ipv4Addr::from(net)
                    },
                    config.network.subnet_prefix
                ),
                config.network.gateway_ip,
            );

            let vm_config = VmManagerConfig {
                kernel_path: config.vm.kernel_path.clone(),
                initrd_path: config.vm.initrd_path.clone(),
                run_dir: config.vm.run_dir.clone(),
                kernel_cmdline: config.vm.kernel_append.clone(),
                init_mode: config.vm.init_mode.clone(),
                initrd_fast_path: config.vm.initrd_fast_path.clone(),
                qmp_connect_timeout: std::time::Duration::from_secs(5),
                guest_ready_timeout: std::time::Duration::from_secs(
                    config.vm.boot_timeout_secs,
                ),
                guest_agent_port: config.vm.guest_agent_port,
            };
            let vm = VmManager::new(vm_config);

            // Build workspace manager and initialize
            let pool = crate::workspace::pool::VmPool::new(config.pool.clone());
            let workspace_manager = Arc::new(WorkspaceManager::new(
                config.clone(),
                vm,
                storage,
                network,
                pool,
            ));

            workspace_manager
                .init()
                .await
                .expect("failed to initialize workspace manager");

            // Load persisted state
            if let Err(e) = workspace_manager.load_state().await {
                tracing::warn!(error = %e, "failed to load persisted state, starting fresh");
            }

            // Spawn periodic state persistence
            {
                let wm = Arc::clone(&workspace_manager);
                let interval_secs = config.server.state_persist_interval_secs;
                tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(interval_secs));
                    loop {
                        interval.tick().await;
                        if let Err(e) = wm.save_state().await {
                            tracing::warn!(error = %e, "periodic state save failed");
                        }
                    }
                });
            }

            // Spawn warm pool replenishment task
            if config.pool.enabled {
                let wm = Arc::clone(&workspace_manager);
                tokio::spawn(async move {
                    // Short initial delay so the MCP server can start accepting
                    // connections while the first pool VM boots.
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(2));
                    loop {
                        interval.tick().await;
                        if let Err(e) = wm.replenish_pool().await {
                            tracing::warn!(error = %e, "pool replenishment failed");
                        }
                    }
                });
                tracing::info!("warm pool replenishment task started");
            }

            tracing::info!("agentiso ready, starting MCP server");

            // Start MCP server, but also listen for termination signals
            // so we always get a chance to clean up.
            let serve_result = tokio::select! {
                result = mcp::serve(workspace_manager.clone(), config.server.transfer_dir.clone()) => {
                    result
                }
                _ = async {
                    let mut sigterm = tokio::signal::unix::signal(
                        tokio::signal::unix::SignalKind::terminate(),
                    ).expect("failed to register SIGTERM handler");
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            tracing::info!("received SIGINT, initiating shutdown");
                        }
                        _ = sigterm.recv() => {
                            tracing::info!("received SIGTERM, initiating shutdown");
                        }
                    }
                } => {
                    Ok(())
                }
            };

            // Graceful shutdown: always run regardless of serve() outcome.
            tracing::info!("MCP server exited, shutting down workspaces");
            workspace_manager.shutdown_all().await;
            workspace_manager.save_state().await.ok();
            tracing::info!("agentiso shut down");

            // Propagate any MCP serve error after cleanup.
            serve_result?;
        }
    }

    Ok(())
}
