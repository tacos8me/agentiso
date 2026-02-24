use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use super::DashboardState;

/// Constant-time string comparison to prevent timing side-channel attacks.
/// XORs all bytes and accumulates differences; the result reveals nothing
/// about *which* byte differed.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let result = a
        .as_bytes()
        .iter()
        .zip(b.as_bytes().iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    result == 0
}

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

    // Check Authorization header (constant-time comparison to prevent timing attacks)
    if let Some(auth_header) = request.headers().get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(bearer_token) = auth_str.strip_prefix("Bearer ") {
                if constant_time_eq(bearer_token, token) {
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
    use super::*;

    #[test]
    fn auth_module_compiles() {
        // Compilation test — actual middleware tested via integration tests
    }

    #[test]
    fn constant_time_eq_equal_strings() {
        assert!(constant_time_eq("secret-token-123", "secret-token-123"));
    }

    #[test]
    fn constant_time_eq_different_strings() {
        assert!(!constant_time_eq("secret-token-123", "secret-token-124"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq("short", "much-longer-string"));
    }

    #[test]
    fn constant_time_eq_empty_strings() {
        assert!(constant_time_eq("", ""));
    }
}
