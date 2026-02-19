mod cli;
mod config;
mod dashboard;
mod guest;
mod init;
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
    /// One-shot environment setup: prerequisites, ZFS, networking, kernel, guest agent, config.
    Init {
        /// Skip interactive confirmations.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Path to use for ZFS pool backing (disk device or sparse file path).
        #[arg(long)]
        pool_path: Option<PathBuf>,
        /// Custom kernel path (default: /boot/vmlinuz-$(uname -r)).
        #[arg(long)]
        kernel: Option<PathBuf>,
        /// Custom initrd path (default: /boot/initrd.img-$(uname -r)).
        #[arg(long)]
        initrd: Option<PathBuf>,
    },
    /// Start the MCP server (stdio transport).
    Serve {
        /// Path to config file (TOML).
        #[arg(long, short)]
        config: Option<PathBuf>,
        /// Port for Prometheus metrics and health endpoints (/metrics, /healthz).
        /// Disabled if not set.
        #[arg(long)]
        metrics_port: Option<u16>,
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
    /// Run batch AI coding tasks across parallel worker VMs.
    Orchestrate {
        /// Path to TOML task file defining the orchestration plan.
        #[arg(long)]
        task_file: PathBuf,
        /// Environment variable name containing the API key (default: ANTHROPIC_API_KEY).
        #[arg(long, default_value = "ANTHROPIC_API_KEY")]
        api_key_env: String,
        /// Maximum number of concurrent worker VMs (default: 10).
        #[arg(long, default_value = "10")]
        max_parallel: usize,
        /// Show the execution plan without running anything.
        #[arg(long)]
        dry_run: bool,
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
        Commands::Init { yes, pool_path, kernel, initrd } => {
            init::run_init(&init::InitOptions {
                yes,
                pool_path,
                kernel,
                initrd,
            })?;
        }
        Commands::Dashboard { config: config_path, refresh } => {
            let config = cli::load_config(config_path)?;
            dashboard::run(config, refresh)?;
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
        Commands::Orchestrate {
            task_file,
            api_key_env,
            max_parallel,
            dry_run,
            config: config_path,
        } => {
            use crate::workspace::orchestrate;

            // Load and validate the plan
            let plan = orchestrate::load_plan(&task_file)?;

            if dry_run {
                orchestrate::print_dry_run(&plan);
                return Ok(());
            }

            // Read API key from environment
            let api_key = std::env::var(&api_key_env).with_context(|| {
                format!(
                    "API key not found in environment variable '{}'. \
                     Set it or use --api-key-env to specify a different variable.",
                    api_key_env
                )
            })?;
            if api_key.trim().is_empty() {
                anyhow::bail!(
                    "API key in environment variable '{}' is empty. \
                     Set a valid key or use --api-key-env to specify a different variable.",
                    api_key_env
                );
            }

            // Build workspace manager (same pattern as serve)
            let config = match config_path {
                Some(path) => Config::load(&path)?,
                None => Config::default(),
            };

            // Acquire exclusive instance lock to prevent concurrent orchestrate
            // instances sharing the same state file, IP allocator, and nftables table.
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
            // lock_file must stay alive for the entire orchestrate duration (dropped at end of block)

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

            if let Err(e) = workspace_manager.load_state().await {
                tracing::warn!(error = %e, "failed to load persisted state, starting fresh");
            }

            // Initialize vault manager for context resolution
            let vault_manager = crate::mcp::vault::VaultManager::new(&config.vault);
            if vault_manager.is_some() {
                tracing::info!(path = %config.vault.path.display(), "vault enabled for orchestration");
            }

            // Execute orchestration with SIGINT handler
            println!("Starting orchestration: {} tasks from '{}'",
                plan.tasks.len(), plan.golden_workspace);

            let wm_for_cleanup = workspace_manager.clone();
            let outcome = tokio::select! {
                result = orchestrate::execute(
                    workspace_manager.clone(),
                    &plan,
                    &api_key,
                    max_parallel,
                    vault_manager,
                ) => { result? }
                _ = tokio::signal::ctrl_c() => {
                    tracing::warn!("received SIGINT during orchestration, cleaning up worker VMs");
                    eprintln!("\nInterrupted! Cleaning up worker VMs...");
                    // Scan for orch-* worker VMs and destroy them
                    if let Ok(all_ws) = wm_for_cleanup.list().await {
                        let orch_workers: Vec<_> = all_ws.iter()
                            .filter(|ws| ws.name.starts_with("orch-"))
                            .collect();
                        if !orch_workers.is_empty() {
                            eprintln!("Destroying {} worker VMs...", orch_workers.len());
                            for ws in &orch_workers {
                                if let Err(e) = wm_for_cleanup.destroy(ws.id).await {
                                    tracing::warn!(id = %ws.id, error = %e, "failed to destroy worker VM during SIGINT cleanup");
                                }
                            }
                        }
                    }
                    wm_for_cleanup.save_state().await.ok();
                    anyhow::bail!("orchestration interrupted by SIGINT");
                }
            };

            // Save results
            let output_dir = std::path::PathBuf::from("./orchestrate-results");
            let result_dir = orchestrate::save_results(&outcome.result, &output_dir).await?;

            // Print summary table
            println!();
            println!("=== Orchestration Results ===");
            println!();
            println!("{:<30} {:<10} {:<10}", "TASK", "STATUS", "EXIT CODE");
            println!("{}", "-".repeat(50));
            for task in &outcome.result.tasks {
                let status = if task.success { "OK" } else { "FAILED" };
                println!("{:<30} {:<10} {:<10}", task.name, status, task.exit_code);
            }
            println!();
            println!(
                "Total: {} succeeded, {} failed",
                outcome.result.success_count, outcome.result.failure_count
            );
            println!("Results saved to: {}", result_dir.display());

            // Persist state after cleanup
            workspace_manager.save_state().await.ok();
        }
        Commands::Serve { config: config_path, metrics_port } => {
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

            // Create auth manager and wire it to workspace manager for session persistence
            let auth_manager = crate::mcp::auth::AuthManager::new(
                crate::mcp::auth::SessionQuota::default(),
            );
            workspace_manager
                .set_auth_manager(std::sync::Arc::new(auth_manager.clone()))
                .await;

            workspace_manager
                .init()
                .await
                .expect("failed to initialize workspace manager");

            // Load persisted state (also restores session ownership via auth manager)
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

            // Optionally start metrics HTTP server
            let metrics = if let Some(port) = metrics_port {
                let m = mcp::metrics::MetricsRegistry::new();
                workspace_manager.set_metrics(m.clone()).await;
                let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
                mcp::metrics::start_metrics_server(
                    addr,
                    m.clone(),
                    Arc::clone(&workspace_manager),
                );
                Some(m)
            } else {
                None
            };

            // Initialize vault manager if configured
            let vault_manager = mcp::vault::VaultManager::new(&config.vault);
            if vault_manager.is_some() {
                tracing::info!(path = %config.vault.path.display(), "vault enabled");
            }

            tracing::info!("agentiso ready, starting MCP server");

            // Start MCP server, but also listen for termination signals
            // so we always get a chance to clean up.
            let serve_result = tokio::select! {
                result = mcp::serve(workspace_manager.clone(), auth_manager, config.server.transfer_dir.clone(), metrics, vault_manager, config.rate_limit.clone()) => {
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
