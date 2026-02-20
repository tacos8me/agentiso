use agentiso_protocol::*;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use std::os::fd::{AsRawFd, FromRawFd};
use std::pin::Pin;
use std::task::Poll;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;
use tracing::{error, info, warn};

pub mod daemon;

/// In-memory inbox for messages pushed from the host relay.
static MESSAGE_INBOX: std::sync::OnceLock<Arc<TokioMutex<Vec<agentiso_protocol::TeamMessageEnvelope>>>> =
    std::sync::OnceLock::new();

fn message_inbox() -> &'static Arc<TokioMutex<Vec<agentiso_protocol::TeamMessageEnvelope>>> {
    MESSAGE_INBOX.get_or_init(|| Arc::new(TokioMutex::new(Vec::new())))
}

/// Daemon result outbox — completed task results awaiting host collection.
static RESULT_OUTBOX: std::sync::OnceLock<daemon::ResultOutbox> = std::sync::OnceLock::new();

fn result_outbox() -> &'static daemon::ResultOutbox {
    RESULT_OUTBOX.get_or_init(daemon::ResultOutbox::new)
}

/// Daemon execution state — tracks pending task count.
static DAEMON_STATE: std::sync::OnceLock<daemon::DaemonState> = std::sync::OnceLock::new();

fn daemon_state() -> &'static daemon::DaemonState {
    DAEMON_STATE.get_or_init(daemon::DaemonState::new)
}

async fn read_message<R: AsyncReadExt + Unpin, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<T> {
    let len = reader
        .read_u32()
        .await
        .context("failed to read message length")?;
    if len > MAX_MESSAGE_SIZE {
        bail!(
            "message too large: {} bytes (max {})",
            len,
            MAX_MESSAGE_SIZE
        );
    }
    let mut buf = vec![0u8; len as usize];
    reader
        .read_exact(&mut buf)
        .await
        .context("failed to read message body")?;
    let msg = serde_json::from_slice(&buf).context("failed to deserialize message")?;
    Ok(msg)
}

async fn write_message<W: AsyncWriteExt + Unpin, T: Serialize>(
    writer: &mut W,
    msg: &T,
) -> Result<()> {
    let encoded = encode_message(msg)?;
    writer
        .write_all(&encoded)
        .await
        .context("failed to write message")?;
    writer.flush().await.context("failed to flush")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Request handlers
// ---------------------------------------------------------------------------

static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn uptime_secs() -> u64 {
    START_TIME
        .get()
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0)
}

async fn handle_ping() -> GuestResponse {
    GuestResponse::Pong(PongResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: uptime_secs(),
    })
}

async fn handle_exec(req: ExecRequest) -> GuestResponse {
    let mut cmd = if req.args.is_empty() {
        // Shell mode: interpret command as a shell string
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&req.command);
        c
    } else {
        // Direct exec mode: command is the program, args are its arguments
        let mut c = Command::new(&req.command);
        c.args(&req.args);
        c
    };

    // Apply persistent env vars first, then per-request overrides
    {
        let store = env_store().lock().await;
        for (key, val) in store.iter() {
            cmd.env(key, val);
        }
    }
    for (key, val) in &req.env {
        cmd.env(key, val);
    }

    if let Some(ref dir) = req.workdir {
        cmd.current_dir(dir);
    }

    // Redirect stdin to /dev/null so children don't inherit the vsock fd,
    // and pipe stdout/stderr so we can read them.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::IoError,
                message: format!("failed to execute command: {e}"),
            });
        }
    };

    let dur = std::time::Duration::from_secs(req.timeout_secs);

    // Take stdout/stderr handles before waiting so we can read them on success
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    match tokio::time::timeout(dur, child.wait()).await {
        Ok(Ok(status)) => {
            // Child exited within timeout - read remaining stdout/stderr
            let stdout = if let Some(mut out) = stdout_handle {
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf).await;
                buf.truncate(MAX_EXEC_OUTPUT_BYTES);
                String::from_utf8_lossy(&buf).into_owned()
            } else {
                String::new()
            };
            let stderr = if let Some(mut err) = stderr_handle {
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
                buf.truncate(MAX_EXEC_OUTPUT_BYTES);
                String::from_utf8_lossy(&buf).into_owned()
            } else {
                String::new()
            };
            GuestResponse::ExecResult(ExecResponse {
                exit_code: status.code().unwrap_or(-1),
                stdout,
                stderr,
            })
        }
        Ok(Err(e)) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::IoError,
            message: format!("failed to wait on command: {e}"),
        }),
        Err(_) => {
            // Timeout - kill the child process and reap it
            let _ = child.kill().await;
            let _ = child.wait().await;
            GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Timeout,
                message: format!("command timed out after {}s", req.timeout_secs),
            })
        }
    }
}

async fn handle_file_read(req: FileReadRequest) -> GuestResponse {
    let path = Path::new(&req.path);
    if !path.exists() {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::NotFound,
            message: format!("file not found: {}", req.path),
        });
    }

    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() > 32 * 1024 * 1024 => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!("file too large: {} bytes (max 32 MiB)", meta.len()),
            });
        }
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::IoError,
                message: format!("failed to stat file: {e}"),
            });
        }
        Ok(_) => {}
    }

    match tokio::fs::read(path).await {
        Ok(data) => {
            let offset = req.offset.unwrap_or(0) as usize;
            let slice = if offset >= data.len() {
                &[]
            } else {
                let end = req
                    .limit
                    .map(|l| (offset + l as usize).min(data.len()))
                    .unwrap_or(data.len());
                &data[offset..end]
            };
            use base64::Engine;
            GuestResponse::FileContent(FileContentResponse {
                content: base64::engine::general_purpose::STANDARD.encode(slice),
                size: slice.len() as u64,
            })
        }
        Err(e) => GuestResponse::Error(ErrorResponse {
            code: if e.kind() == std::io::ErrorKind::PermissionDenied {
                ErrorCode::PermissionDenied
            } else {
                ErrorCode::IoError
            },
            message: format!("failed to read file: {e}"),
        }),
    }
}

async fn handle_file_write(req: FileWriteRequest) -> GuestResponse {
    use base64::Engine;
    let data = match base64::engine::general_purpose::STANDARD.decode(&req.content) {
        Ok(d) => d,
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!("invalid base64 content: {e}"),
            });
        }
    };

    let path = Path::new(&req.path);

    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::IoError,
                message: format!("failed to create parent directories: {e}"),
            });
        }
    }

    if let Err(e) = tokio::fs::write(path, &data).await {
        return GuestResponse::Error(ErrorResponse {
            code: if e.kind() == std::io::ErrorKind::PermissionDenied {
                ErrorCode::PermissionDenied
            } else {
                ErrorCode::IoError
            },
            message: format!("failed to write file: {e}"),
        });
    }

    if let Some(mode) = req.mode {
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
    }

    GuestResponse::Ok
}

async fn handle_file_upload(req: FileUploadRequest) -> GuestResponse {
    use base64::Engine;
    let data = match base64::engine::general_purpose::STANDARD.decode(&req.data) {
        Ok(d) => d,
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!("invalid base64 data: {e}"),
            });
        }
    };

    let path = Path::new(&req.guest_path);

    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::IoError,
                message: format!("failed to create parent directories: {e}"),
            });
        }
    }

    if let Err(e) = tokio::fs::write(path, &data).await {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::IoError,
            message: format!("failed to write uploaded file: {e}"),
        });
    }

    if let Some(mode) = req.mode {
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
    }

    GuestResponse::Ok
}

async fn handle_file_download(req: FileDownloadRequest) -> GuestResponse {
    let path = Path::new(&req.guest_path);
    if !path.exists() {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::NotFound,
            message: format!("file not found: {}", req.guest_path),
        });
    }

    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() > 32 * 1024 * 1024 => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!("file too large: {} bytes (max 32 MiB)", meta.len()),
            });
        }
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::IoError,
                message: format!("failed to stat file: {e}"),
            });
        }
        Ok(_) => {}
    }

    match tokio::fs::read(path).await {
        Ok(data) => {
            use base64::Engine;
            let size = data.len() as u64;
            GuestResponse::FileData(FileDataResponse {
                data: base64::engine::general_purpose::STANDARD.encode(&data),
                size,
            })
        }
        Err(e) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::IoError,
            message: format!("failed to read file for download: {e}"),
        }),
    }
}

async fn handle_configure_network(cfg: NetworkConfig) -> GuestResponse {
    // Strip existing CIDR if present, then validate IP format
    let ip_addr = match parse_bare_ipv4(&cfg.ip_address) {
        Some(ip) => ip,
        None => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!("invalid IP address: {}", cfg.ip_address),
            });
        }
    };
    let ip_cidr = format!("{}/16", ip_addr);

    // Configure the eth0 interface with the assigned IP.
    match Command::new("ip")
        .args(["addr", "flush", "dev", "eth0"])
        .output()
        .await
    {
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!("failed to flush eth0: {e}"),
            });
        }
        Ok(output) if !output.status.success() => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!(
                    "ip addr flush failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(_) => {}
    }

    match Command::new("ip")
        .args(["addr", "add", &ip_cidr, "dev", "eth0"])
        .output()
        .await
    {
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!("failed to add IP address: {e}"),
            });
        }
        Ok(output) if !output.status.success() => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!(
                    "ip addr add failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(_) => {}
    }

    match Command::new("ip")
        .args(["link", "set", "eth0", "up"])
        .output()
        .await
    {
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!("failed to bring up eth0: {e}"),
            });
        }
        Ok(output) if !output.status.success() => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!(
                    "ip link set up failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(_) => {}
    }

    match Command::new("ip")
        .args(["route", "add", "default", "via", &cfg.gateway])
        .output()
        .await
    {
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::Internal,
                message: format!("failed to add default route: {e}"),
            });
        }
        Ok(output) if !output.status.success() => {
            // Retry once after a short delay -- the interface may not be fully ready
            let first_err = String::from_utf8_lossy(&output.stderr).to_string();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            match Command::new("ip")
                .args(["route", "add", "default", "via", &cfg.gateway])
                .output()
                .await
            {
                Ok(retry_output) if retry_output.status.success() => { /* retry succeeded */ }
                _ => {
                    return GuestResponse::Error(ErrorResponse {
                        code: ErrorCode::Internal,
                        message: format!("ip route add failed: {}", first_err),
                    });
                }
            }
        }
        Ok(_) => {}
    }

    // Write /etc/resolv.conf
    let resolv_content: String = cfg.dns.iter().map(|d| format!("nameserver {d}\n")).collect();
    if let Err(e) = tokio::fs::write("/etc/resolv.conf", resolv_content).await {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::Internal,
            message: format!("failed to write /etc/resolv.conf: {e}"),
        });
    }

    GuestResponse::Ok
}

fn is_valid_hostname(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

async fn handle_set_hostname(req: SetHostnameRequest) -> GuestResponse {
    if !is_valid_hostname(&req.hostname) {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: format!("invalid hostname: {:?}", req.hostname),
        });
    }

    let result = Command::new("hostname")
        .arg(&req.hostname)
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            // Also persist to /etc/hostname
            let _ = tokio::fs::write("/etc/hostname", format!("{}\n", req.hostname)).await;
            GuestResponse::Ok
        }
        Ok(output) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::Internal,
            message: format!(
                "hostname command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        }),
        Err(e) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::Internal,
            message: format!("failed to set hostname: {e}"),
        }),
    }
}

async fn handle_configure_workspace(cfg: WorkspaceConfig) -> GuestResponse {
    // Validate IP address before proceeding
    if parse_bare_ipv4(&cfg.ip_address).is_none() {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: format!("invalid IP address: {}", cfg.ip_address),
        });
    }

    // Validate hostname
    if !is_valid_hostname(&cfg.hostname) {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: format!("invalid hostname: {:?}", cfg.hostname),
        });
    }

    // Configure network (same logic as handle_configure_network)
    let net_cfg = NetworkConfig {
        ip_address: cfg.ip_address,
        gateway: cfg.gateway,
        dns: cfg.dns,
    };
    let net_result = handle_configure_network(net_cfg).await;
    if matches!(net_result, GuestResponse::Error(_)) {
        return net_result;
    }

    // Set hostname
    let hostname_result = handle_set_hostname(SetHostnameRequest {
        hostname: cfg.hostname,
    }).await;
    if matches!(hostname_result, GuestResponse::Error(_)) {
        return hostname_result;
    }

    GuestResponse::Ok
}

// ---------------------------------------------------------------------------
// Background job tracking
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicU32, Ordering};
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Persistent environment variables (set via SetEnv, applied to Exec)
// ---------------------------------------------------------------------------

static ENV_STORE: std::sync::OnceLock<Mutex<HashMap<String, String>>> =
    std::sync::OnceLock::new();

fn env_store() -> &'static Mutex<HashMap<String, String>> {
    ENV_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Env var names that must not be overridden for security reasons.
/// Any name starting with `LD_` is blocked to prevent dynamic linker attacks.
const DANGEROUS_ENV_NAMES: &[&str] = &["PATH", "IFS", "ENV", "BASH_ENV"];

/// Prefixes that are blocked for security reasons.
const DANGEROUS_ENV_PREFIXES: &[&str] = &["LD_"];

/// Validate that an env var name contains only alphanumeric chars and underscores,
/// and does not start with a digit.
fn is_valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.as_bytes()[0].is_ascii_digit()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Returns true if the env var name is blocked for security reasons.
fn is_dangerous_env_name(name: &str) -> bool {
    DANGEROUS_ENV_NAMES.contains(&name)
        || DANGEROUS_ENV_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Strip a CIDR suffix (e.g. "/16") from an IP string and validate it as IPv4.
/// Returns the bare IP on success, or None on invalid input.
fn parse_bare_ipv4(ip: &str) -> Option<std::net::Ipv4Addr> {
    let bare = ip.split('/').next().unwrap_or(ip);
    bare.parse::<std::net::Ipv4Addr>().ok()
}

static JOB_COUNTER: AtomicU32 = AtomicU32::new(1);

struct JobState {
    done: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    pid: Option<u32>,
}

static JOBS: std::sync::OnceLock<Mutex<HashMap<u32, JobState>>> = std::sync::OnceLock::new();

fn jobs() -> &'static Mutex<HashMap<u32, JobState>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// New request handlers
// ---------------------------------------------------------------------------

fn format_permissions(mode: u32) -> String {
    let chars = ['x', 'w', 'r'];
    let mut s = String::with_capacity(9);
    for shift in (0..9).rev() {
        s.push(if mode & (1 << shift) != 0 { chars[shift % 3] } else { '-' });
    }
    s
}

async fn handle_list_dir(req: ListDirRequest) -> GuestResponse {
    match tokio::fs::read_dir(&req.path).await {
        Err(e) => GuestResponse::Error(ErrorResponse {
            code: if e.kind() == std::io::ErrorKind::NotFound {
                ErrorCode::NotFound
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                ErrorCode::PermissionDenied
            } else {
                ErrorCode::IoError
            },
            message: format!("list_dir failed: {e}"),
        }),
        Ok(mut dir) => {
            let mut entries = Vec::new();
            while let Ok(Some(entry)) = dir.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                let meta = match entry.metadata().await {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let kind = if meta.is_dir() {
                    "dir"
                } else if meta.is_symlink() {
                    "symlink"
                } else {
                    "file"
                }
                .to_string();
                let size = meta.len();
                let mode = meta.permissions().mode();
                let permissions = format_permissions(mode);
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                entries.push(DirEntry {
                    name,
                    kind,
                    size,
                    permissions,
                    modified,
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            GuestResponse::DirListing(DirListingResponse { entries })
        }
    }
}

async fn handle_edit_file(req: EditFileRequest) -> GuestResponse {
    let content = match tokio::fs::read_to_string(&req.path).await {
        Err(e) => {
            return GuestResponse::Error(ErrorResponse {
                code: if e.kind() == std::io::ErrorKind::NotFound {
                    ErrorCode::NotFound
                } else {
                    ErrorCode::IoError
                },
                message: format!("read failed: {e}"),
            });
        }
        Ok(c) => c,
    };
    if !content.contains(&req.old_string) {
        return GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: format!("old_string not found in {}", req.path),
        });
    }
    let new_content = content.replacen(&req.old_string, &req.new_string, 1);
    match tokio::fs::write(&req.path, new_content).await {
        Err(e) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::IoError,
            message: format!("write failed: {e}"),
        }),
        Ok(_) => GuestResponse::Ok,
    }
}

const MAX_BACKGROUND_JOBS: usize = 1000;

/// Maximum bytes to capture from stdout or stderr of a command.
/// Prevents unbounded memory growth if a command produces excessive output.
const MAX_EXEC_OUTPUT_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

async fn handle_exec_background(req: ExecBackgroundRequest) -> GuestResponse {
    let job_id = JOB_COUNTER.fetch_add(1, Ordering::SeqCst);
    {
        let mut map = jobs().lock().await;
        if map.len() >= MAX_BACKGROUND_JOBS {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!(
                    "too many background jobs ({} active, max {})",
                    map.len(),
                    MAX_BACKGROUND_JOBS
                ),
            });
        }
        map.insert(
            job_id,
            JobState {
                done: false,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                pid: None,
            },
        );
    }

    // Spawn the child process manually so we can track its PID for kill support.
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(&req.command);
    if let Some(ref dir) = req.workdir {
        cmd.current_dir(dir);
    }
    // Apply persistent env vars first, then per-request overrides
    {
        let store = env_store().lock().await;
        for (k, v) in store.iter() {
            cmd.env(k, v);
        }
    }
    if let Some(ref env_map) = req.env {
        for (k, v) in env_map {
            cmd.env(k, v);
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Put the child in its own process group so that exec_kill can send
    // signals to the entire group (kill(-pgid, sig)), ensuring child
    // processes spawned by the shell are also terminated.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Remove the job entry since we failed to spawn
            let mut map = jobs().lock().await;
            map.remove(&job_id);
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::IoError,
                message: format!("failed to spawn background command: {e}"),
            });
        }
    };

    // Store the child PID in the job state
    let child_pid = child.id();
    {
        let mut map = jobs().lock().await;
        if let Some(state) = map.get_mut(&job_id) {
            state.pid = child_pid;
        }
    }

    // Take stdout/stderr handles before moving child into the spawned task
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();

    tokio::spawn(async move {
        // Read stdout and stderr concurrently, then wait for exit
        let stdout_fut = async {
            if let Some(mut out) = stdout_handle {
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut buf).await;
                buf.truncate(MAX_EXEC_OUTPUT_BYTES);
                String::from_utf8_lossy(&buf).into_owned()
            } else {
                String::new()
            }
        };
        let stderr_fut = async {
            if let Some(mut err) = stderr_handle {
                let mut buf = Vec::new();
                let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut buf).await;
                buf.truncate(MAX_EXEC_OUTPUT_BYTES);
                String::from_utf8_lossy(&buf).into_owned()
            } else {
                String::new()
            }
        };

        let (stdout, stderr) = tokio::join!(stdout_fut, stderr_fut);
        let wait_result = child.wait().await;

        let mut map = jobs().lock().await;
        if let Some(state) = map.get_mut(&job_id) {
            match wait_result {
                Ok(status) => {
                    state.exit_code = status.code();
                    state.stdout = stdout;
                    state.stderr = stderr;
                }
                Err(e) => {
                    state.stderr = format!("wait failed: {e}");
                    state.exit_code = Some(-1);
                }
            }
            state.done = true;
        }
    });
    GuestResponse::BackgroundStarted(BackgroundStartedResponse { job_id })
}

async fn handle_exec_poll(req: ExecPollRequest) -> GuestResponse {
    let mut map = jobs().lock().await;
    // Check if the job exists and whether it is done
    let is_done = map.get(&req.job_id).map(|s| s.done);
    match is_done {
        None => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::NotFound,
            message: format!("unknown job_id: {}", req.job_id),
        }),
        Some(true) => {
            // Job completed: remove from map and return final status
            let state = map.remove(&req.job_id).unwrap();
            GuestResponse::BackgroundStatus(BackgroundStatusResponse {
                running: false,
                exit_code: state.exit_code,
                stdout: state.stdout,
                stderr: state.stderr,
            })
        }
        Some(false) => {
            // Job still running: return current status, leave in map
            let state = map.get(&req.job_id).unwrap();
            GuestResponse::BackgroundStatus(BackgroundStatusResponse {
                running: true,
                exit_code: state.exit_code,
                stdout: state.stdout.clone(),
                stderr: state.stderr.clone(),
            })
        }
    }
}

async fn handle_exec_kill(req: ExecKillRequest) -> GuestResponse {
    // Look up job state under lock, extract what we need, then release.
    let lookup = {
        let map = jobs().lock().await;
        match map.get(&req.job_id) {
            None => None,
            Some(state) => Some((state.done, state.pid)),
        }
    };

    match lookup {
        None => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::NotFound,
            message: format!("unknown job_id: {}", req.job_id),
        }),
        Some((true, _)) => {
            // Already done, just acknowledge
            GuestResponse::Ok
        }
        Some((false, Some(pid))) => {
            // Send the signal to the process group (negative PID) so that any
            // child processes spawned by the shell are also killed. This is
            // important because /bin/sh may fork children that inherit the
            // stdout/stderr pipe fds — killing only the shell would leave
            // those children alive and the pipes open, preventing the
            // background reaper task from detecting completion.
            //
            // Try process-group kill first; fall back to single-process kill
            // if the process is not a group leader (ESRCH on negative PID).
            let ret = unsafe { libc::kill(-(pid as i32), req.signal) };
            let ret = if ret != 0 {
                // Process group kill failed — try single-process kill
                unsafe { libc::kill(pid as i32, req.signal) }
            } else {
                ret
            };
            if ret == 0 {
                // Signal was delivered. Wait briefly for the background reaper
                // task to notice the process death and mark the job as done.
                // This avoids a race where exec_poll is called immediately
                // after exec_kill and sees stale running=true state.
                for _ in 0..50 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    let map = jobs().lock().await;
                    match map.get(&req.job_id) {
                        Some(state) if state.done => break,
                        None => break, // already removed
                        _ => {}
                    }
                }
                GuestResponse::Ok
            } else {
                GuestResponse::Error(ErrorResponse {
                    code: ErrorCode::IoError,
                    message: format!(
                        "kill({}, {}) failed: {}",
                        pid,
                        req.signal,
                        std::io::Error::last_os_error()
                    ),
                })
            }
        }
        Some((false, None)) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::Internal,
            message: "job has no tracked PID".to_string(),
        }),
    }
}

async fn handle_set_env(req: SetEnvRequest) -> GuestResponse {
    // Validate all var names before storing any
    for name in req.vars.keys() {
        if !is_valid_env_name(name) {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!(
                    "invalid env var name {:?}: must be alphanumeric and underscores only",
                    name
                ),
            });
        }
        if is_dangerous_env_name(name) {
            return GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: format!(
                    "env var {:?} is not allowed to be overridden",
                    name
                ),
            });
        }
    }

    let count = req.vars.len();
    let mut store = env_store().lock().await;
    for (key, val) in req.vars {
        store.insert(key, val);
    }
    GuestResponse::SetEnvResult(SetEnvResponse { count })
}

async fn handle_request(req: GuestRequest) -> GuestResponse {
    match req {
        GuestRequest::Ping => handle_ping().await,
        GuestRequest::Exec(r) => handle_exec(r).await,
        GuestRequest::FileRead(r) => handle_file_read(r).await,
        GuestRequest::FileWrite(r) => handle_file_write(r).await,
        GuestRequest::FileUpload(r) => handle_file_upload(r).await,
        GuestRequest::FileDownload(r) => handle_file_download(r).await,
        GuestRequest::ConfigureNetwork(cfg) => handle_configure_network(cfg).await,
        GuestRequest::SetHostname(r) => handle_set_hostname(r).await,
        GuestRequest::ConfigureWorkspace(cfg) => handle_configure_workspace(cfg).await,
        GuestRequest::ListDir(r) => handle_list_dir(r).await,
        GuestRequest::EditFile(r) => handle_edit_file(r).await,
        GuestRequest::ExecBackground(r) => handle_exec_background(r).await,
        GuestRequest::ExecPoll(r) => handle_exec_poll(r).await,
        GuestRequest::ExecKill(r) => handle_exec_kill(r).await,
        GuestRequest::SetEnv(r) => handle_set_env(r).await,
        GuestRequest::VaultRead(_)
        | GuestRequest::VaultWrite(_)
        | GuestRequest::VaultSearch(_)
        | GuestRequest::VaultList(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "vault operations are handled by the host, not the guest agent".to_string(),
        }),
        GuestRequest::TaskClaim(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "task claims are handled by the host, not the guest agent".to_string(),
        }),
        GuestRequest::TeamMessage(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "TeamMessage must be sent to the host relay, not handled by guest".to_string(),
        }),
        GuestRequest::TeamReceive(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "TeamReceive must be sent to the host relay, not handled by guest".to_string(),
        }),
        GuestRequest::CreateSubTeam(_) => GuestResponse::Error(ErrorResponse {
            code: ErrorCode::InvalidRequest,
            message: "CreateSubTeam must be initiated via host MCP tool, not handled by guest"
                .to_string(),
        }),
        GuestRequest::PollDaemonResults(req) => {
            let results = result_outbox().drain(req.limit as usize);
            let pending = daemon_state().pending_count();
            GuestResponse::DaemonResults(DaemonResultsResponse { results, pending_tasks: pending })
        }
        GuestRequest::Shutdown => {
            info!("shutdown requested, initiating poweroff");
            // Spawn poweroff in background so we can send the response first.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let _ = Command::new("poweroff").output().await;
            });
            GuestResponse::Ok
        }
    }
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

async fn handle_connection(
    mut reader: impl AsyncReadExt + Unpin,
    mut writer: impl AsyncWriteExt + Unpin,
    peer: String,
) {
    info!(peer = %peer, "new connection");

    loop {
        let req: GuestRequest = match read_message(&mut reader).await {
            Ok(r) => r,
            Err(e) => {
                // EOF or connection reset is expected on clean disconnect.
                let is_disconnect = e.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .map(|io_err| matches!(
                            io_err.kind(),
                            std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::BrokenPipe
                        ))
                        .unwrap_or(false)
                });
                if is_disconnect {
                    info!(peer = %peer, "connection closed");
                } else {
                    warn!(peer = %peer, error = %e, "failed to read request");
                }
                return;
            }
        };

        let response = handle_request(req).await;

        if let Err(e) = write_message(&mut writer, &response).await {
            error!(peer = %peer, error = %e, "failed to write response");
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP API (port 8080)
// ---------------------------------------------------------------------------

use axum::{routing::get, routing::post, Json, Router};

/// Start the HTTP API server on port 8080 (best-effort, non-fatal).
async fn start_http_api() {
    let app = Router::new()
        .route("/health", get(http_health))
        .route("/messages", get(http_get_messages))
        .route("/messages", post(http_post_message));

    let addr = "0.0.0.0:8080";
    info!(addr, "starting HTTP API");
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => {
            if let Err(e) = axum::serve(listener, app).await {
                error!(error = %e, "HTTP API server error");
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to bind HTTP API on {}", addr);
        }
    }
}

async fn http_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ready",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": uptime_secs(),
    }))
}

async fn http_get_messages() -> Json<serde_json::Value> {
    let mut inbox = message_inbox().lock().await;
    let messages: Vec<_> = inbox.drain(..).collect();
    Json(serde_json::json!({
        "messages": messages,
    }))
}

async fn http_post_message(
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    // Accept messages posted directly by peers
    let envelope = agentiso_protocol::TeamMessageEnvelope {
        message_id: uuid::Uuid::new_v4().to_string(),
        from: body.get("from").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
        to: "self".to_string(),
        content: body.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        message_type: body.get("message_type").and_then(|v| v.as_str()).unwrap_or("text").to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    let mut inbox = message_inbox().lock().await;
    if inbox.len() >= 1000 {
        inbox.remove(0);
    }
    inbox.push(envelope);

    Json(serde_json::json!({ "status": "accepted" }))
}

// ---------------------------------------------------------------------------
// Relay connection handler (port 5001)
// ---------------------------------------------------------------------------

/// Handle a relay connection from the host.
///
/// The host sends GuestRequest messages and expects GuestResponse back.
/// For TeamMessage requests received on this channel, the guest stores
/// them in the local message inbox (they were relayed from another agent).
async fn handle_relay_connection(
    mut reader: impl AsyncReadExt + Unpin,
    mut writer: impl AsyncWriteExt + Unpin,
    peer: String,
) {
    info!(peer = %peer, "new relay connection");

    loop {
        let req: GuestRequest = match read_message(&mut reader).await {
            Ok(r) => r,
            Err(e) => {
                let is_disconnect = e.chain().any(|cause| {
                    cause
                        .downcast_ref::<std::io::Error>()
                        .map(|io_err| matches!(
                            io_err.kind(),
                            std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::BrokenPipe
                        ))
                        .unwrap_or(false)
                });
                if is_disconnect {
                    info!(peer = %peer, "relay connection closed");
                } else {
                    warn!(peer = %peer, error = %e, "failed to read relay message");
                }
                return;
            }
        };

        // On the relay channel, we handle Ping (for keepalive) and
        // use a special convention: host sends TeamMessage as a "push"
        // to deliver a message from another agent. The guest stores it.
        let response = match req {
            GuestRequest::Ping => handle_ping().await,
            GuestRequest::TeamMessage(tm) => {
                // Host is pushing a message from another agent to us.
                // Use propagated `from` and `message_id` fields if present,
                // falling back to legacy behaviour for backwards compatibility.
                let envelope = agentiso_protocol::TeamMessageEnvelope {
                    message_id: if tm.message_id.is_empty() {
                        uuid::Uuid::new_v4().to_string()
                    } else {
                        tm.message_id
                    },
                    from: if tm.from.is_empty() {
                        // Legacy fallback: 'to' field was repurposed as sender name
                        tm.to.clone()
                    } else {
                        tm.from
                    },
                    to: "self".to_string(),
                    content: tm.content,
                    message_type: tm.message_type,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
                let mut inbox = message_inbox().lock().await;
                // Cap at 1000 messages
                if inbox.len() >= 1000 {
                    inbox.remove(0);
                }
                inbox.push(envelope);
                GuestResponse::Ok
            }
            _ => GuestResponse::Error(ErrorResponse {
                code: ErrorCode::InvalidRequest,
                message: "relay channel only accepts Ping and TeamMessage".to_string(),
            }),
        };

        if let Err(e) = write_message(&mut writer, &response).await {
            error!(peer = %peer, error = %e, "failed to write relay response");
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Vsock listener
// ---------------------------------------------------------------------------

const AF_VSOCK: i32 = 40;

#[repr(C)]
struct SockaddrVm {
    svm_family: u16,
    svm_reserved1: u16,
    svm_port: u32,
    svm_cid: u32,
    svm_flags: u8,
    svm_zero: [u8; 3],
}

/// A vsock listener that accepts connections using raw syscalls,
/// avoiding TcpListener's address parsing which fails for AF_VSOCK.
struct VsockListener {
    async_fd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}

impl VsockListener {
    fn bind(port: u32) -> Result<Self> {
        use std::os::fd::FromRawFd;

        let fd = unsafe {
            libc::socket(
                AF_VSOCK,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            bail!(
                "socket(AF_VSOCK) failed: {}",
                std::io::Error::last_os_error()
            );
        }

        let addr = SockaddrVm {
            svm_family: AF_VSOCK as u16,
            svm_reserved1: 0,
            svm_port: port,
            svm_cid: u32::MAX, // VMADDR_CID_ANY
            svm_flags: 0,
            svm_zero: [0; 3],
        };

        let ret = unsafe {
            libc::bind(
                fd,
                &addr as *const SockaddrVm as *const libc::sockaddr,
                std::mem::size_of::<SockaddrVm>() as u32,
            )
        };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            bail!("bind(vsock port {port}) failed: {err}");
        }

        let ret = unsafe { libc::listen(fd, 128) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            bail!("listen(vsock) failed: {err}");
        }

        let owned_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let async_fd = tokio::io::unix::AsyncFd::new(owned_fd)?;
        Ok(Self { async_fd })
    }

    /// Accept a vsock connection, returning a VsockStream.
    async fn accept(&self) -> Result<(VsockStream, u32)> {
        loop {
            let mut guard = self.async_fd.readable().await?;

            // Use try_io — the recommended AsyncFd pattern. It automatically
            // clears readiness on WouldBlock and retains it on success.
            match guard.try_io(|inner| {
                let client_fd = unsafe {
                    libc::accept4(
                        inner.get_ref().as_raw_fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                    )
                };
                if client_fd >= 0 {
                    Ok(client_fd)
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }) {
                Ok(Ok(client_fd)) => {
                    // Get peer CID for logging
                    let mut peer_addr: SockaddrVm = unsafe { std::mem::zeroed() };
                    let mut addr_len = std::mem::size_of::<SockaddrVm>() as u32;
                    unsafe {
                        libc::getpeername(
                            client_fd,
                            &mut peer_addr as *mut SockaddrVm as *mut libc::sockaddr,
                            &mut addr_len,
                        );
                    }
                    let peer_cid = peer_addr.svm_cid;

                    // Wrap in AsyncFd directly — NOT UnixStream, which expects AF_UNIX
                    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(client_fd) };
                    let stream = VsockStream::new(owned)?;
                    return Ok((stream, peer_cid));
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_would_block) => continue,
            }
        }
    }
}

/// An async vsock stream backed by `AsyncFd<OwnedFd>` with raw read/write.
///
/// We cannot wrap vsock fds in `tokio::net::UnixStream` because mio's internal
/// bookkeeping expects `AF_UNIX` semantics (e.g. `getpeername` with `sockaddr_un`).
/// Instead we implement `AsyncRead`/`AsyncWrite` directly via `libc::read`/`libc::write`.
struct VsockStream {
    inner: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}

impl VsockStream {
    fn new(fd: std::os::fd::OwnedFd) -> std::io::Result<Self> {
        let inner = tokio::io::unix::AsyncFd::new(fd)?;
        Ok(Self { inner })
    }
}

impl tokio::io::AsyncRead for VsockStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            let mut guard = match self.inner.poll_read_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            let unfilled = buf.initialize_unfilled();
            match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let n = unsafe {
                    libc::read(fd, unfilled.as_mut_ptr() as *mut libc::c_void, unfilled.len())
                };
                if n >= 0 {
                    Ok(n as usize)
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl tokio::io::AsyncWrite for VsockStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        loop {
            let mut guard = match self.inner.poll_write_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            match guard.try_io(|inner| {
                let fd = inner.get_ref().as_raw_fd();
                let n = unsafe {
                    libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len())
                };
                if n >= 0 {
                    Ok(n as usize)
                } else {
                    Err(std::io::Error::last_os_error())
                }
            }) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        let fd = self.inner.get_ref().as_raw_fd();
        let ret = unsafe { libc::shutdown(fd, libc::SHUT_WR) };
        if ret == 0 {
            Poll::Ready(Ok(()))
        } else {
            Poll::Ready(Err(std::io::Error::last_os_error()))
        }
    }
}

enum Listener {
    Vsock(VsockListener),
    Tcp(TcpListener),
}

/// Load vsock kernel modules via insmod. Called when socket(AF_VSOCK) fails,
/// indicating the modules aren't loaded yet (common with Alpine + microvm).
fn load_vsock_modules() {
    let kver = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    info!(kernel = %kver, "loading vsock kernel modules");

    let kdir = format!("/lib/modules/{kver}/kernel/net/vmw_vsock");

    for module in &[
        "vsock.ko",
        "vmw_vsock_virtio_transport_common.ko",
        "vmw_vsock_virtio_transport.ko",
    ] {
        let path = format!("{kdir}/{module}");
        match std::process::Command::new("insmod").arg(&path).output() {
            Ok(output) if output.status.success() => {
                info!(module = %module, "loaded kernel module");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("File exists") {
                    info!(module = %module, "kernel module already loaded");
                } else {
                    warn!(module = %module, error = %stderr.trim(), path = %path, "insmod failed");
                }
            }
            Err(e) => {
                warn!(module = %module, error = %e, "failed to run insmod");
            }
        }
    }

    // Log the local CID to verify transport is working
    match std::fs::read_to_string("/sys/class/vsock/local_cid") {
        Ok(cid) => info!(local_cid = %cid.trim(), "vsock transport active"),
        Err(e) => warn!(error = %e, "cannot read vsock local CID — transport may not be active"),
    }
}

/// Try vsock first; fall back to TCP for development without a vsock kernel.
async fn listen(port: u32) -> Result<Listener> {
    // First attempt — modules may already be loaded (e.g. by init script)
    match VsockListener::bind(port) {
        Ok(listener) => {
            info!(port, "listening on vsock");
            return Ok(Listener::Vsock(listener));
        }
        Err(e) => {
            info!(error = %e, "vsock not available, loading kernel modules...");
        }
    }

    // Load vsock modules ourselves and retry with tight polling
    load_vsock_modules();
    for attempt in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        match VsockListener::bind(port) {
            Ok(listener) => {
                info!(port, attempt, "listening on vsock (after module load)");
                return Ok(Listener::Vsock(listener));
            }
            Err(_) if attempt < 19 => continue,
            Err(e) => {
                warn!(error = %e, port, "vsock still unavailable after module load, falling back to TCP");
                let addr = format!("0.0.0.0:{port}");
                let listener = TcpListener::bind(&addr)
                    .await
                    .with_context(|| format!("failed to bind TCP fallback on {addr}"))?;
                info!(addr = %addr, "listening on TCP (fallback)");
                return Ok(Listener::Tcp(listener));
            }
        }
    }
    unreachable!()
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    START_TIME.get_or_init(Instant::now);

    info!(
        version = env!("CARGO_PKG_VERSION"),
        "agentiso guest agent starting"
    );

    let listener = listen(GUEST_AGENT_PORT).await?;

    // Start HTTP API (best-effort, non-fatal)
    tokio::spawn(start_http_api());

    // Start agent daemon (watches MESSAGE_INBOX for task assignments)
    let inbox_ref = message_inbox();
    tokio::spawn(async move {
        daemon::run(result_outbox(), daemon_state(), inbox_ref).await;
    });
    info!("agent daemon started");

    // Start relay listener on port 5001 (best-effort; non-fatal if bind fails)
    tokio::spawn(async {
        match listen(agentiso_protocol::RELAY_PORT).await {
            Ok(listener) => {
                info!(port = agentiso_protocol::RELAY_PORT, "relay listener started");
                match listener {
                    Listener::Vsock(vsock) => {
                        loop {
                            match vsock.accept().await {
                                Ok((stream, peer_cid)) => {
                                    let peer = format!("relay:vsock:cid={peer_cid}");
                                    let (reader, writer) = tokio::io::split(stream);
                                    tokio::spawn(async move {
                                        handle_relay_connection(reader, writer, peer).await;
                                    });
                                }
                                Err(e) => {
                                    warn!(error = %e, "relay accept failed");
                                }
                            }
                        }
                    }
                    Listener::Tcp(tcp) => {
                        loop {
                            match tcp.accept().await {
                                Ok((stream, addr)) => {
                                    let peer = format!("relay:tcp:{addr}");
                                    let (reader, writer) = stream.into_split();
                                    tokio::spawn(async move {
                                        handle_relay_connection(reader, writer, peer).await;
                                    });
                                }
                                Err(e) => {
                                    warn!(error = %e, "relay TCP accept failed");
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to start relay listener (team messaging disabled)");
            }
        }
    });

    match listener {
        Listener::Vsock(vsock) => {
            loop {
                let (stream, peer_cid) = vsock.accept().await?;
                let peer = format!("vsock:cid={peer_cid}");
                let (reader, writer) = tokio::io::split(stream);
                tokio::spawn(async move {
                    handle_connection(reader, writer, peer).await;
                });
            }
        }
        Listener::Tcp(tcp) => {
            loop {
                let (stream, addr) = tcp.accept().await?;
                let peer = format!("tcp:{addr}");
                let (reader, writer) = stream.into_split();
                tokio::spawn(async move {
                    handle_connection(reader, writer, peer).await;
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // is_valid_env_name
    // -----------------------------------------------------------------------

    #[test]
    fn valid_env_names() {
        assert!(is_valid_env_name("FOO"));
        assert!(is_valid_env_name("MY_VAR"));
        assert!(is_valid_env_name("A123"));
        assert!(is_valid_env_name("_PRIVATE"));
        assert!(is_valid_env_name("a"));
    }

    #[test]
    fn invalid_env_name_empty() {
        assert!(!is_valid_env_name(""));
    }

    #[test]
    fn invalid_env_name_starts_with_digit() {
        assert!(!is_valid_env_name("123ABC"));
        assert!(!is_valid_env_name("9VAR"));
    }

    #[test]
    fn invalid_env_name_special_chars() {
        assert!(!is_valid_env_name("FOO BAR"));
        assert!(!is_valid_env_name("FOO=BAR"));
        assert!(!is_valid_env_name("FOO-BAR"));
        assert!(!is_valid_env_name("FOO.BAR"));
    }

    // -----------------------------------------------------------------------
    // is_dangerous_env_name
    // -----------------------------------------------------------------------

    #[test]
    fn blocked_env_names_exact() {
        assert!(is_dangerous_env_name("PATH"));
        assert!(is_dangerous_env_name("IFS"));
    }

    #[test]
    fn blocked_env_names_ld_prefix() {
        assert!(is_dangerous_env_name("LD_PRELOAD"));
        assert!(is_dangerous_env_name("LD_LIBRARY_PATH"));
        assert!(is_dangerous_env_name("LD_ANYTHING"));
    }

    #[test]
    fn safe_env_names_not_blocked() {
        assert!(!is_dangerous_env_name("HOME"));
        assert!(!is_dangerous_env_name("ANTHROPIC_API_KEY"));
        assert!(!is_dangerous_env_name("MY_VAR"));
        // "PATHOLOGICAL" starts with PATH but is not exactly "PATH"
        assert!(!is_dangerous_env_name("PATHOLOGICAL"));
    }

    // -----------------------------------------------------------------------
    // parse_bare_ipv4
    // -----------------------------------------------------------------------

    #[test]
    fn parse_bare_ipv4_plain() {
        let ip = parse_bare_ipv4("10.42.0.5");
        assert_eq!(ip, Some(std::net::Ipv4Addr::new(10, 42, 0, 5)));
    }

    #[test]
    fn parse_bare_ipv4_with_cidr() {
        let ip = parse_bare_ipv4("10.42.0.5/16");
        assert_eq!(ip, Some(std::net::Ipv4Addr::new(10, 42, 0, 5)));
    }

    #[test]
    fn parse_bare_ipv4_invalid() {
        assert!(parse_bare_ipv4("not-an-ip").is_none());
        assert!(parse_bare_ipv4("").is_none());
        assert!(parse_bare_ipv4("999.999.999.999").is_none());
        assert!(parse_bare_ipv4("10.42.0").is_none());
    }

    // -----------------------------------------------------------------------
    // is_valid_hostname
    // -----------------------------------------------------------------------

    #[test]
    fn valid_hostnames() {
        assert!(is_valid_hostname("ws-abc12345"));
        assert!(is_valid_hostname("myhost"));
        assert!(is_valid_hostname("a"));
        assert!(is_valid_hostname("host-name-123"));
    }

    #[test]
    fn invalid_hostname_empty() {
        assert!(!is_valid_hostname(""));
    }

    #[test]
    fn invalid_hostname_too_long() {
        let long = "a".repeat(64);
        assert!(!is_valid_hostname(&long));
        // 63 is the max
        let max = "a".repeat(63);
        assert!(is_valid_hostname(&max));
    }

    #[test]
    fn invalid_hostname_leading_trailing_dash() {
        assert!(!is_valid_hostname("-leading"));
        assert!(!is_valid_hostname("trailing-"));
        assert!(!is_valid_hostname("-both-"));
    }

    #[test]
    fn invalid_hostname_special_chars() {
        assert!(!is_valid_hostname("host.name"));
        assert!(!is_valid_hostname("host_name"));
        assert!(!is_valid_hostname("host name"));
    }

    // -----------------------------------------------------------------------
    // format_permissions
    // -----------------------------------------------------------------------

    #[test]
    fn format_permissions_rwxrwxrwx() {
        assert_eq!(format_permissions(0o777), "rwxrwxrwx");
    }

    #[test]
    fn format_permissions_rwxr_xr_x() {
        assert_eq!(format_permissions(0o755), "rwxr-xr-x");
    }

    #[test]
    fn format_permissions_rw_r_r() {
        assert_eq!(format_permissions(0o644), "rw-r--r--");
    }

    #[test]
    fn format_permissions_none() {
        assert_eq!(format_permissions(0o000), "---------");
    }

    // -----------------------------------------------------------------------
    // handle_set_env (async tests via tokio)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn set_env_valid_vars() {
        let mut vars = HashMap::new();
        vars.insert("MY_KEY".to_string(), "my_value".to_string());
        let req = SetEnvRequest { vars };
        let resp = handle_set_env(req).await;
        match resp {
            GuestResponse::SetEnvResult(r) => assert_eq!(r.count, 1),
            other => panic!("expected SetEnvResult, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn set_env_rejects_invalid_name() {
        let mut vars = HashMap::new();
        vars.insert("INVALID NAME".to_string(), "val".to_string());
        let req = SetEnvRequest { vars };
        let resp = handle_set_env(req).await;
        assert!(matches!(resp, GuestResponse::Error(_)));
    }

    #[tokio::test]
    async fn set_env_rejects_dangerous_path() {
        let mut vars = HashMap::new();
        vars.insert("PATH".to_string(), "/evil".to_string());
        let req = SetEnvRequest { vars };
        let resp = handle_set_env(req).await;
        match &resp {
            GuestResponse::Error(e) => {
                assert!(e.message.contains("not allowed"), "msg: {}", e.message);
            }
            other => panic!("expected Error, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn set_env_rejects_ld_preload() {
        let mut vars = HashMap::new();
        vars.insert("LD_PRELOAD".to_string(), "/evil.so".to_string());
        let req = SetEnvRequest { vars };
        let resp = handle_set_env(req).await;
        assert!(matches!(resp, GuestResponse::Error(_)));
    }

    // -----------------------------------------------------------------------
    // handle_ping
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn ping_returns_pong() {
        START_TIME.get_or_init(Instant::now);
        let resp = handle_ping().await;
        match resp {
            GuestResponse::Pong(p) => {
                assert_eq!(p.version, env!("CARGO_PKG_VERSION"));
            }
            other => panic!("expected Pong, got {:?}", other),
        }
    }

    #[test]
    fn blocked_env_names_shell_startup() {
        assert!(is_dangerous_env_name("ENV"));
        assert!(is_dangerous_env_name("BASH_ENV"));
    }

    #[test]
    fn max_exec_output_bytes_is_reasonable() {
        assert_eq!(MAX_EXEC_OUTPUT_BYTES, 2 * 1024 * 1024);
    }
}
