use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::{EncodeLabelSet, EncodeLabelValue};
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;

use crate::workspace::WorkspaceManager;

// ---------------------------------------------------------------------------
// Label types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct WorkspaceStateLabels {
    state: WorkspaceStateLabel,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
enum WorkspaceStateLabel {
    Running,
    Stopped,
    Suspended,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ExecResultLabels {
    result: ExecResultLabel,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
enum ExecResultLabel {
    Success,
    Error,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ErrorTypeLabels {
    error_type: String,
}

// ---------------------------------------------------------------------------
// MetricsRegistry
// ---------------------------------------------------------------------------

/// Prometheus metrics registry for agentiso.
///
/// All methods are cheap (atomic operations) and safe to call from any async context.
/// The struct is `Clone + Send + Sync` via internal `Arc`.
#[derive(Clone)]
pub struct MetricsRegistry {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    registry: Mutex<Registry>,
    workspaces_total: Family<WorkspaceStateLabels, Gauge>,
    vm_boot_duration_seconds: Histogram,
    exec_total: Family<ExecResultLabels, Counter>,
    exec_duration_seconds: Histogram,
    errors_total: Family<ErrorTypeLabels, Counter>,
    start_time: Instant,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let workspaces_total = Family::<WorkspaceStateLabels, Gauge>::default();
        registry.register(
            "agentiso_workspaces_total",
            "Current number of workspaces by state",
            workspaces_total.clone(),
        );

        // Boot duration: buckets from 0.1s to ~25s (exponential)
        let vm_boot_duration_seconds =
            Histogram::new(exponential_buckets(0.1, 2.0, 8));
        registry.register(
            "agentiso_vm_boot_duration_seconds",
            "VM boot duration in seconds",
            vm_boot_duration_seconds.clone(),
        );

        let exec_total = Family::<ExecResultLabels, Counter>::default();
        registry.register(
            "agentiso_exec_total",
            "Total exec commands by result",
            exec_total.clone(),
        );

        // Exec duration: buckets from 0.01s to ~2.5s (exponential)
        let exec_duration_seconds =
            Histogram::new(exponential_buckets(0.01, 2.0, 8));
        registry.register(
            "agentiso_exec_duration_seconds",
            "Exec command duration in seconds",
            exec_duration_seconds.clone(),
        );

        let errors_total = Family::<ErrorTypeLabels, Counter>::default();
        registry.register(
            "agentiso_errors_total",
            "Total errors by type",
            errors_total.clone(),
        );

        Self {
            inner: Arc::new(MetricsInner {
                registry: Mutex::new(registry),
                workspaces_total,
                vm_boot_duration_seconds,
                exec_total,
                exec_duration_seconds,
                errors_total,
                start_time: Instant::now(),
            }),
        }
    }

    /// Record a VM boot duration.
    pub fn record_boot(&self, duration: std::time::Duration) {
        self.inner
            .vm_boot_duration_seconds
            .observe(duration.as_secs_f64());
    }

    /// Record an exec command completion.
    pub fn record_exec(&self, duration: std::time::Duration, exit_code: i32) {
        self.inner
            .exec_duration_seconds
            .observe(duration.as_secs_f64());
        let result = if exit_code == 0 {
            ExecResultLabel::Success
        } else {
            ExecResultLabel::Error
        };
        self.inner
            .exec_total
            .get_or_create(&ExecResultLabels { result })
            .inc();
    }

    /// Increment the error counter for a given error type.
    pub fn record_error(&self, error_type: &str) {
        self.inner
            .errors_total
            .get_or_create(&ErrorTypeLabels {
                error_type: error_type.to_string(),
            })
            .inc();
    }

    /// Update workspace count gauges.
    pub fn set_workspace_counts(&self, running: i64, stopped: i64, suspended: i64) {
        self.inner
            .workspaces_total
            .get_or_create(&WorkspaceStateLabels {
                state: WorkspaceStateLabel::Running,
            })
            .set(running);
        self.inner
            .workspaces_total
            .get_or_create(&WorkspaceStateLabels {
                state: WorkspaceStateLabel::Stopped,
            })
            .set(stopped);
        self.inner
            .workspaces_total
            .get_or_create(&WorkspaceStateLabels {
                state: WorkspaceStateLabel::Suspended,
            })
            .set(suspended);
    }

    /// Encode all metrics in OpenMetrics text format.
    pub fn encode_metrics(&self) -> String {
        let mut buf = String::new();
        let registry = self.inner.registry.lock().unwrap();
        encode(&mut buf, &registry).unwrap();
        buf
    }

    /// Get uptime in seconds since the registry was created.
    pub fn uptime_seconds(&self) -> u64 {
        self.inner.start_time.elapsed().as_secs()
    }
}

// ---------------------------------------------------------------------------
// HTTP server (axum)
// ---------------------------------------------------------------------------

struct MetricsState {
    metrics: MetricsRegistry,
    workspace_manager: Arc<WorkspaceManager>,
}

async fn metrics_handler(State(state): State<Arc<MetricsState>>) -> impl IntoResponse {
    // Refresh workspace counts before encoding
    if let Ok(workspaces) = state.workspace_manager.list().await {
        let mut running = 0i64;
        let mut stopped = 0i64;
        let mut suspended = 0i64;
        for ws in &workspaces {
            match ws.state {
                crate::workspace::WorkspaceState::Running => running += 1,
                crate::workspace::WorkspaceState::Stopped => stopped += 1,
                crate::workspace::WorkspaceState::Suspended => suspended += 1,
            }
        }
        state.metrics.set_workspace_counts(running, stopped, suspended);
    }

    let body = state.metrics.encode_metrics();
    (
        StatusCode::OK,
        [("content-type", "application/openmetrics-text; version=1.0.0; charset=utf-8")],
        body,
    )
}

async fn healthz_handler(State(state): State<Arc<MetricsState>>) -> impl IntoResponse {
    let (running, stopped, suspended) = if let Ok(workspaces) = state.workspace_manager.list().await
    {
        let mut r = 0u64;
        let mut s = 0u64;
        let mut su = 0u64;
        for ws in &workspaces {
            match ws.state {
                crate::workspace::WorkspaceState::Running => r += 1,
                crate::workspace::WorkspaceState::Stopped => s += 1,
                crate::workspace::WorkspaceState::Suspended => su += 1,
            }
        }
        (r, s, su)
    } else {
        (0, 0, 0)
    };

    let body = serde_json::json!({
        "status": "ok",
        "workspaces": {
            "running": running,
            "stopped": stopped,
            "suspended": suspended,
        },
        "uptime_seconds": state.metrics.uptime_seconds(),
    });

    (
        StatusCode::OK,
        [("content-type", "application/json")],
        body.to_string(),
    )
}

/// Start the metrics HTTP server as a background tokio task.
///
/// Returns the `JoinHandle` so the caller can optionally await it.
pub fn start_metrics_server(
    addr: SocketAddr,
    metrics: MetricsRegistry,
    workspace_manager: Arc<WorkspaceManager>,
) -> tokio::task::JoinHandle<()> {
    let state = Arc::new(MetricsState {
        metrics,
        workspace_manager,
    });

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(state);

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(addr = %addr, error = %e, "failed to bind metrics server");
                return;
            }
        };
        tracing::info!(addr = %addr, "metrics server listening");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "metrics server error");
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_registry() {
        let reg = MetricsRegistry::new();
        let text = reg.encode_metrics();
        // Should contain our metric names
        assert!(text.contains("agentiso_workspaces_total"));
        assert!(text.contains("agentiso_vm_boot_duration_seconds"));
        assert!(text.contains("agentiso_exec_total"));
        assert!(text.contains("agentiso_exec_duration_seconds"));
        assert!(text.contains("agentiso_errors_total"));
    }

    #[test]
    fn test_record_boot() {
        let reg = MetricsRegistry::new();
        reg.record_boot(std::time::Duration::from_millis(1500));
        let text = reg.encode_metrics();
        assert!(text.contains("agentiso_vm_boot_duration_seconds"));
    }

    #[test]
    fn test_record_exec() {
        let reg = MetricsRegistry::new();
        reg.record_exec(std::time::Duration::from_millis(100), 0);
        reg.record_exec(std::time::Duration::from_millis(200), 1);
        let text = reg.encode_metrics();
        assert!(text.contains("agentiso_exec_total"));
        assert!(text.contains("Success"));
        assert!(text.contains("Error"));
    }

    #[test]
    fn test_record_error() {
        let reg = MetricsRegistry::new();
        reg.record_error("vm_launch");
        reg.record_error("vm_launch");
        reg.record_error("vsock_timeout");
        let text = reg.encode_metrics();
        assert!(text.contains("vm_launch"));
        assert!(text.contains("vsock_timeout"));
    }

    #[test]
    fn test_set_workspace_counts() {
        let reg = MetricsRegistry::new();
        reg.set_workspace_counts(3, 1, 0);
        let text = reg.encode_metrics();
        assert!(text.contains("Running"));
        assert!(text.contains("Stopped"));
    }

    #[test]
    fn test_uptime() {
        let reg = MetricsRegistry::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Uptime should be at least 0
        assert!(reg.uptime_seconds() < 2);
    }

    #[test]
    fn test_clone_is_shared() {
        let reg1 = MetricsRegistry::new();
        let reg2 = reg1.clone();
        reg1.record_error("test_error");
        // Both clones should see the same data
        let text = reg2.encode_metrics();
        assert!(text.contains("test_error"));
    }
}
