use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use super::DashboardState;

/// Auth middleware that checks the admin token.
///
/// Skips authentication when:
/// - admin_token is empty (default, safe for localhost)
/// - request comes from a loopback address and no token is configured
///
/// When admin_token is set, requires `Authorization: Bearer <token>` header.
pub async fn auth_middleware(
    State(state): State<Arc<DashboardState>>,
    request: Request,
    next: Next,
) -> Response {
    let token = &state.config.dashboard.admin_token;

    // No auth required when token is empty
    if token.is_empty() {
        return next.run(request).await;
    }

    // Check Authorization header
    if let Some(auth_header) = request.headers().get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(bearer_token) = auth_str.strip_prefix("Bearer ") {
                if bearer_token == token {
                    return next.run(request).await;
                }
            }
        }
    }

    super::error_response(
        axum::http::StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "invalid or missing admin token",
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn auth_module_compiles() {
        // Compilation test — actual middleware tested via integration tests
    }
}
