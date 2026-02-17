mod config;
mod guest;
mod mcp;
mod network;
mod storage;
mod vm;
mod workspace;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { config: config_path } => {
            let config = match config_path {
                Some(path) => Config::load(&path)?,
                None => Config::default(),
            };
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
                init_mode: "openrc".into(),
                initrd_fast_path: None,
                qmp_connect_timeout: std::time::Duration::from_secs(5),
                guest_ready_timeout: std::time::Duration::from_secs(
                    config.vm.boot_timeout_secs,
                ),
                guest_agent_port: config.vm.guest_agent_port,
            };
            let vm = VmManager::new(vm_config);

            // Build workspace manager and initialize
            let workspace_manager = Arc::new(WorkspaceManager::new(
                config.clone(),
                vm,
                storage,
                network,
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
