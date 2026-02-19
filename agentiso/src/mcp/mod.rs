pub mod auth;
pub(crate) mod git_tools;
pub mod metrics;
pub mod rate_limit;
pub mod tools;
pub mod vault;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;
use tracing::info;

use crate::workspace::WorkspaceManager;

use crate::config::RateLimitConfig;
use self::auth::AuthManager;
use self::metrics::MetricsRegistry;
use self::tools::AgentisoServer;

/// Start the MCP server on stdio transport.
pub async fn serve(
    workspace_manager: Arc<WorkspaceManager>,
    auth_manager: AuthManager,
    transfer_dir: PathBuf,
    metrics: Option<MetricsRegistry>,
    vault_manager: Option<Arc<vault::VaultManager>>,
    rate_limit_config: RateLimitConfig,
) -> Result<()> {
    // Ensure the transfer directory exists.
    tokio::fs::create_dir_all(&transfer_dir).await?;

    // Generate a session ID for this stdio connection.
    let session_id = uuid::Uuid::new_v4().to_string();
    auth_manager.register_session(session_id.clone()).await;

    let server = AgentisoServer::with_metrics(
        workspace_manager,
        auth_manager,
        session_id,
        transfer_dir,
        metrics,
        vault_manager,
        rate_limit_config,
    );

    info!("starting MCP server on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
