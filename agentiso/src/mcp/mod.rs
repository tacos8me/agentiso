pub mod auth;
pub mod tools;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use rmcp::transport::io::stdio;
use tracing::info;

use crate::workspace::WorkspaceManager;

use self::auth::{AuthManager, SessionQuota};
use self::tools::AgentisoServer;

/// Start the MCP server on stdio transport.
pub async fn serve(
    workspace_manager: Arc<WorkspaceManager>,
    transfer_dir: PathBuf,
) -> Result<()> {
    let auth_manager = AuthManager::new(SessionQuota::default());

    // Ensure the transfer directory exists.
    tokio::fs::create_dir_all(&transfer_dir).await?;

    // Generate a session ID for this stdio connection.
    let session_id = uuid::Uuid::new_v4().to_string();
    auth_manager.register_session(session_id.clone()).await;

    let server = AgentisoServer::new(
        workspace_manager,
        auth_manager,
        session_id,
        transfer_dir,
    );

    info!("starting MCP server on stdio");
    let service = server.serve(stdio()).await?;
    service.waiting().await?;

    Ok(())
}
