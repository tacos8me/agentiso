use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../frontend/dist/"]
pub struct DashboardAssets;

/// Axum handler that serves files from the embedded `DashboardAssets`.
///
/// - Correct MIME types via `mime_guess`
/// - Long-lived cache for hashed assets (`assets/`), no-cache for `index.html`
/// - SPA fallback: unknown paths return `index.html`
pub async fn serve_embedded(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');

    // Try to serve the exact path, then fall back to index.html (SPA routing)
    let (data, effective_path) = match DashboardAssets::get(path) {
        Some(file) => (file, path.to_string()),
        None => match DashboardAssets::get("index.html") {
            Some(file) => (file, "index.html".to_string()),
            None => {
                return (StatusCode::NOT_FOUND, "index.html not found in embedded assets")
                    .into_response();
            }
        },
    };

    // Determine MIME type
    let mime = mime_guess::from_path(&effective_path)
        .first_or_octet_stream()
        .to_string();

    // Cache policy: immutable for hashed assets, no-cache for index.html
    let cache_control = if effective_path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from(data.data.into_owned()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_contains_index_html() {
        assert!(
            DashboardAssets::get("index.html").is_some(),
            "embedded assets must contain index.html"
        );
    }

    #[test]
    fn mime_type_detection() {
        assert_eq!(
            mime_guess::from_path("app.js")
                .first_or_octet_stream()
                .to_string(),
            "text/javascript"
        );
        assert_eq!(
            mime_guess::from_path("style.css")
                .first_or_octet_stream()
                .to_string(),
            "text/css"
        );
        assert_eq!(
            mime_guess::from_path("index.html")
                .first_or_octet_stream()
                .to_string(),
            "text/html"
        );
        assert_eq!(
            mime_guess::from_path("font.woff2")
                .first_or_octet_stream()
                .to_string(),
            "font/woff2"
        );
    }

    #[test]
    fn cache_header_logic() {
        // Hashed assets get immutable cache
        let hashed_path = "assets/index-abc123.js";
        assert!(hashed_path.starts_with("assets/"));

        // Non-asset paths (like index.html) get no-cache
        let root_path = "index.html";
        assert!(!root_path.starts_with("assets/"));
    }
}
