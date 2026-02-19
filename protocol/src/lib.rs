use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vsock port the guest agent listens on. Used by the host-side vsock client.
pub const GUEST_AGENT_PORT: u32 = 5000;

/// Maximum message size (16 MiB) to prevent unbounded allocations.
pub const MAX_MESSAGE_SIZE: u32 = 16 * 1024 * 1024;

/// Framing: each message is a 4-byte big-endian length prefix followed by JSON bytes.
/// The length prefix encodes the size of the JSON payload only (not including itself).

// ---------------------------------------------------------------------------
// Host -> Guest requests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GuestRequest {
    /// Handshake / health check.
    Ping,

    /// Execute a command in the guest.
    Exec(ExecRequest),

    /// Read a file from the guest filesystem.
    FileRead(FileReadRequest),

    /// Write a file to the guest filesystem.
    FileWrite(FileWriteRequest),

    /// Upload file data (host -> guest) in a single message.
    FileUpload(FileUploadRequest),

    /// Download file data (guest -> host) in a single message.
    FileDownload(FileDownloadRequest),

    /// Configure guest networking (IP, gateway, DNS).
    ConfigureNetwork(NetworkConfig),

    /// Set the guest hostname.
    SetHostname(SetHostnameRequest),

    /// Configure workspace in one shot (network + hostname).
    ConfigureWorkspace(WorkspaceConfig),

    /// Graceful shutdown request.
    Shutdown,

    /// List directory contents.
    ListDir(ListDirRequest),

    /// Edit a file by replacing an exact string match.
    EditFile(EditFileRequest),

    /// Start a command in the background, returning a job ID.
    ExecBackground(ExecBackgroundRequest),

    /// Poll a background job for its status and output.
    ExecPoll(ExecPollRequest),

    /// Kill a background job by job ID.
    ExecKill(ExecKillRequest),

    /// Set environment variables that persist across Exec calls.
    SetEnv(SetEnvRequest),

    /// Read a note from the host-side vault.
    VaultRead(VaultReadRequest),
    /// Write a note to the host-side vault.
    VaultWrite(VaultWriteRequest),
    /// Search the host-side vault.
    VaultSearch(VaultSearchRequest),
    /// List entries in the host-side vault.
    VaultList(VaultListRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadRequest {
    pub path: String,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileWriteRequest {
    pub path: String,
    /// Base64-encoded content.
    pub content: String,
    #[serde(default)]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadRequest {
    pub guest_path: String,
    /// Base64-encoded file data.
    pub data: String,
    #[serde(default)]
    pub mode: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadRequest {
    pub guest_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    pub ip_address: String,
    pub gateway: String,
    #[serde(default = "default_dns")]
    pub dns: Vec<String>,
}

fn default_dns() -> Vec<String> {
    vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetHostnameRequest {
    pub hostname: String,
}

/// Combined workspace configuration (network + hostname) in a single vsock RTT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub ip_address: String,
    pub gateway: String,
    #[serde(default = "default_dns")]
    pub dns: Vec<String>,
    pub hostname: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListDirRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditFileRequest {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecBackgroundRequest {
    pub command: String,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecPollRequest {
    pub job_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecKillRequest {
    pub job_id: u32,
    /// Signal number to send (default: 9 = SIGKILL)
    #[serde(default = "default_kill_signal")]
    pub signal: i32,
}

fn default_kill_signal() -> i32 {
    9
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetEnvRequest {
    pub vars: HashMap<String, String>,
}

// --- Vault proxy (Phase 1: guest vault access) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultReadRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultWriteRequest {
    pub path: String,
    pub content: String,
    /// "overwrite" | "append" | "prepend"
    #[serde(default = "default_write_mode")]
    pub mode: String,
}

fn default_write_mode() -> String {
    "overwrite".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSearchRequest {
    pub query: String,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    pub tag_filter: Option<String>,
}

fn default_max_results() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListRequest {
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: bool,
}

// ---------------------------------------------------------------------------
// Guest -> Host responses
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultContentResponse {
    pub path: String,
    pub content: String,
    pub frontmatter: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSearchResultEntry {
    pub path: String,
    pub line_number: usize,
    pub line: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSearchResponse {
    pub results: Vec<VaultSearchResultEntry>,
    pub total_matches: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListResponse {
    pub entries: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GuestResponse {
    /// Pong reply to Ping.
    Pong(PongResponse),

    /// Result of command execution.
    ExecResult(ExecResponse),

    /// File content read from guest.
    FileContent(FileContentResponse),

    /// Acknowledgment of a write/upload/config operation.
    Ok,

    /// File download data.
    FileData(FileDataResponse),

    /// Error response.
    Error(ErrorResponse),

    /// Directory listing result.
    DirListing(DirListingResponse),

    /// Background job started.
    BackgroundStarted(BackgroundStartedResponse),

    /// Background job status/output.
    BackgroundStatus(BackgroundStatusResponse),

    /// Result of setting environment variables.
    SetEnvResult(SetEnvResponse),

    /// Vault note content.
    VaultContent(VaultContentResponse),
    /// Vault write acknowledgment.
    VaultWriteOk,
    /// Vault search results.
    VaultSearchResults(VaultSearchResponse),
    /// Vault directory listing.
    VaultEntries(VaultListResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PongResponse {
    pub version: String,
    pub uptime_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContentResponse {
    /// Base64-encoded content.
    pub content: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDataResponse {
    /// Base64-encoded file data.
    pub data: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorCode {
    NotFound,
    PermissionDenied,
    Timeout,
    IoError,
    InvalidRequest,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    /// "file", "dir", or "symlink"
    pub kind: String,
    pub size: u64,
    /// Unix permission string, e.g. "rwxr-xr-x"
    pub permissions: String,
    /// Modified time as Unix timestamp (seconds since epoch)
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirListingResponse {
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundStartedResponse {
    pub job_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundStatusResponse {
    pub running: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetEnvResponse {
    /// Number of environment variables that were set.
    pub count: usize,
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// Encode a message as length-prefixed JSON bytes.
pub fn encode_message<T: Serialize>(msg: &T) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(msg)?;
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Decode a length-prefixed JSON message from a byte buffer.
/// Returns the deserialized message and the number of bytes consumed.
pub fn decode_message<T: serde::de::DeserializeOwned>(
    buf: &[u8],
) -> Result<(T, usize), Box<dyn std::error::Error>> {
    if buf.len() < 4 {
        return Err("buffer too short for length prefix".into());
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return Err("buffer too short for payload".into());
    }
    let msg: T = serde_json::from_slice(&buf[4..4 + len])?;
    Ok((msg, 4 + len))
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // GuestRequest serialization round-trips
    // -----------------------------------------------------------------------

    fn roundtrip_request(req: &GuestRequest) -> GuestRequest {
        let json = serde_json::to_string(req).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn test_request_ping_roundtrip() {
        let req = GuestRequest::Ping;
        let rt = roundtrip_request(&req);
        assert!(matches!(rt, GuestRequest::Ping));
    }

    #[test]
    fn test_request_exec_roundtrip() {
        let mut env = HashMap::new();
        env.insert("FOO".into(), "bar".into());
        let req = GuestRequest::Exec(ExecRequest {
            command: "ls".into(),
            args: vec!["-la".into()],
            env,
            workdir: Some("/tmp".into()),
            timeout_secs: 60,
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::Exec(e) = rt {
            assert_eq!(e.command, "ls");
            assert_eq!(e.args, vec!["-la"]);
            assert_eq!(e.env.get("FOO").unwrap(), "bar");
            assert_eq!(e.workdir.as_deref(), Some("/tmp"));
            assert_eq!(e.timeout_secs, 60);
        } else {
            panic!("expected Exec variant");
        }
    }

    #[test]
    fn test_request_file_read_roundtrip() {
        let req = GuestRequest::FileRead(FileReadRequest {
            path: "/etc/passwd".into(),
            offset: Some(100),
            limit: Some(4096),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::FileRead(fr) = rt {
            assert_eq!(fr.path, "/etc/passwd");
            assert_eq!(fr.offset, Some(100));
            assert_eq!(fr.limit, Some(4096));
        } else {
            panic!("expected FileRead variant");
        }
    }

    #[test]
    fn test_request_file_write_roundtrip() {
        let req = GuestRequest::FileWrite(FileWriteRequest {
            path: "/tmp/test.txt".into(),
            content: "aGVsbG8=".into(),
            mode: Some(0o644),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::FileWrite(fw) = rt {
            assert_eq!(fw.path, "/tmp/test.txt");
            assert_eq!(fw.content, "aGVsbG8=");
            assert_eq!(fw.mode, Some(0o644));
        } else {
            panic!("expected FileWrite variant");
        }
    }

    #[test]
    fn test_request_file_upload_roundtrip() {
        let req = GuestRequest::FileUpload(FileUploadRequest {
            guest_path: "/home/user/file.bin".into(),
            data: "AQIDBA==".into(),
            mode: None,
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::FileUpload(fu) = rt {
            assert_eq!(fu.guest_path, "/home/user/file.bin");
            assert_eq!(fu.data, "AQIDBA==");
            assert!(fu.mode.is_none());
        } else {
            panic!("expected FileUpload variant");
        }
    }

    #[test]
    fn test_request_file_download_roundtrip() {
        let req = GuestRequest::FileDownload(FileDownloadRequest {
            guest_path: "/var/log/syslog".into(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::FileDownload(fd) = rt {
            assert_eq!(fd.guest_path, "/var/log/syslog");
        } else {
            panic!("expected FileDownload variant");
        }
    }

    #[test]
    fn test_request_shutdown_roundtrip() {
        let req = GuestRequest::Shutdown;
        let rt = roundtrip_request(&req);
        assert!(matches!(rt, GuestRequest::Shutdown));
    }

    #[test]
    fn test_request_configure_workspace_roundtrip() {
        let req = GuestRequest::ConfigureWorkspace(WorkspaceConfig {
            ip_address: "10.99.0.5/16".into(),
            gateway: "10.99.0.1".into(),
            dns: vec!["1.1.1.1".into()],
            hostname: "ws-abc12345".into(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::ConfigureWorkspace(cfg) = rt {
            assert_eq!(cfg.ip_address, "10.99.0.5/16");
            assert_eq!(cfg.gateway, "10.99.0.1");
            assert_eq!(cfg.hostname, "ws-abc12345");
        } else {
            panic!("expected ConfigureWorkspace variant");
        }
    }

    #[test]
    fn test_request_exec_kill_roundtrip() {
        let req = GuestRequest::ExecKill(ExecKillRequest {
            job_id: 42,
            signal: 15,
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::ExecKill(ek) = rt {
            assert_eq!(ek.job_id, 42);
            assert_eq!(ek.signal, 15);
        } else {
            panic!("expected ExecKill variant");
        }
    }

    #[test]
    fn test_exec_kill_default_signal() {
        // When signal is not in JSON, it should default to 9 (SIGKILL).
        let json = r#"{"type":"ExecKill","job_id":7}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::ExecKill(ek) = req {
            assert_eq!(ek.job_id, 7);
            assert_eq!(ek.signal, 9);
        } else {
            panic!("expected ExecKill variant");
        }
    }

    // -----------------------------------------------------------------------
    // GuestResponse serialization round-trips
    // -----------------------------------------------------------------------

    fn roundtrip_response(resp: &GuestResponse) -> GuestResponse {
        let json = serde_json::to_string(resp).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn test_response_pong_roundtrip() {
        let resp = GuestResponse::Pong(PongResponse {
            version: "0.1.0".into(),
            uptime_secs: 42,
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::Pong(p) = rt {
            assert_eq!(p.version, "0.1.0");
            assert_eq!(p.uptime_secs, 42);
        } else {
            panic!("expected Pong variant");
        }
    }

    #[test]
    fn test_response_exec_result_roundtrip() {
        let resp = GuestResponse::ExecResult(ExecResponse {
            exit_code: 0,
            stdout: "hello world\n".into(),
            stderr: String::new(),
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::ExecResult(e) = rt {
            assert_eq!(e.exit_code, 0);
            assert_eq!(e.stdout, "hello world\n");
            assert!(e.stderr.is_empty());
        } else {
            panic!("expected ExecResult variant");
        }
    }

    #[test]
    fn test_response_ok_roundtrip() {
        let resp = GuestResponse::Ok;
        let rt = roundtrip_response(&resp);
        assert!(matches!(rt, GuestResponse::Ok));
    }

    #[test]
    fn test_response_error_roundtrip() {
        let resp = GuestResponse::Error(ErrorResponse {
            code: ErrorCode::NotFound,
            message: "file not found".into(),
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::Error(e) = rt {
            assert!(matches!(e.code, ErrorCode::NotFound));
            assert_eq!(e.message, "file not found");
        } else {
            panic!("expected Error variant");
        }
    }

    #[test]
    fn test_response_file_content_roundtrip() {
        let resp = GuestResponse::FileContent(FileContentResponse {
            content: "dGVzdA==".into(),
            size: 4,
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::FileContent(fc) = rt {
            assert_eq!(fc.content, "dGVzdA==");
            assert_eq!(fc.size, 4);
        } else {
            panic!("expected FileContent variant");
        }
    }

    #[test]
    fn test_response_file_data_roundtrip() {
        let resp = GuestResponse::FileData(FileDataResponse {
            data: "AQIDBA==".into(),
            size: 4,
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::FileData(fd) = rt {
            assert_eq!(fd.data, "AQIDBA==");
            assert_eq!(fd.size, 4);
        } else {
            panic!("expected FileData variant");
        }
    }

    // -----------------------------------------------------------------------
    // Length-prefixed framing
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_decode_framing() {
        let req = GuestRequest::Ping;
        let encoded = encode_message(&req).unwrap();

        // First 4 bytes are big-endian length.
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        assert_eq!(len as usize, encoded.len() - 4);

        // Decode back.
        let (decoded, consumed): (GuestRequest, usize) = decode_message(&encoded).unwrap();
        assert!(matches!(decoded, GuestRequest::Ping));
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn test_encode_decode_complex_message() {
        let resp = GuestResponse::ExecResult(ExecResponse {
            exit_code: 1,
            stdout: "line1\nline2\n".into(),
            stderr: "error: something failed".into(),
        });
        let encoded = encode_message(&resp).unwrap();
        let (decoded, _): (GuestResponse, usize) = decode_message(&encoded).unwrap();
        if let GuestResponse::ExecResult(e) = decoded {
            assert_eq!(e.exit_code, 1);
            assert_eq!(e.stdout, "line1\nline2\n");
        } else {
            panic!("expected ExecResult variant");
        }
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_strings() {
        let req = GuestRequest::Exec(ExecRequest {
            command: String::new(),
            args: vec![],
            env: HashMap::new(),
            workdir: None,
            timeout_secs: 120,
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::Exec(e) = rt {
            assert!(e.command.is_empty());
            assert!(e.args.is_empty());
        } else {
            panic!("expected Exec variant");
        }
    }

    #[test]
    fn test_special_characters() {
        let req = GuestRequest::FileWrite(FileWriteRequest {
            path: "/tmp/test with spaces & \"quotes\".txt".into(),
            content: "line1\nline2\ttab\r\nnull\0byte".into(),
            mode: None,
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::FileWrite(fw) = rt {
            assert_eq!(fw.path, "/tmp/test with spaces & \"quotes\".txt");
            assert!(fw.content.contains('\0'));
        } else {
            panic!("expected FileWrite variant");
        }
    }

    #[test]
    fn test_large_content() {
        let large = "A".repeat(1024 * 1024); // 1 MiB of data
        let req = GuestRequest::FileWrite(FileWriteRequest {
            path: "/tmp/big".into(),
            content: large.clone(),
            mode: None,
        });
        let encoded = encode_message(&req).unwrap();
        let (decoded, _): (GuestRequest, usize) = decode_message(&encoded).unwrap();
        if let GuestRequest::FileWrite(fw) = decoded {
            assert_eq!(fw.content.len(), 1024 * 1024);
        } else {
            panic!("expected FileWrite variant");
        }
    }

    // -----------------------------------------------------------------------
    // Error code serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_all_error_codes_roundtrip() {
        let codes = vec![
            ErrorCode::NotFound,
            ErrorCode::PermissionDenied,
            ErrorCode::Timeout,
            ErrorCode::IoError,
            ErrorCode::InvalidRequest,
            ErrorCode::Internal,
        ];
        for code in codes {
            let resp = GuestResponse::Error(ErrorResponse {
                code: code.clone(),
                message: format!("test {:?}", code),
            });
            let json = serde_json::to_string(&resp).unwrap();
            let rt: GuestResponse = serde_json::from_str(&json).unwrap();
            if let GuestResponse::Error(e) = rt {
                assert_eq!(format!("{:?}", e.code), format!("{:?}", code));
            } else {
                panic!("expected Error variant");
            }
        }
    }

    #[test]
    fn test_exec_default_timeout() {
        // When timeout_secs is not in JSON, it should default to 120.
        let json = r#"{"type":"Exec","command":"ls","args":[],"env":{}}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::Exec(e) = req {
            assert_eq!(e.timeout_secs, 120);
        } else {
            panic!("expected Exec variant");
        }
    }

    #[test]
    fn test_decode_buffer_too_short() {
        let result = decode_message::<GuestRequest>(&[0, 0]);
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_payload_incomplete() {
        // Length prefix says 100 bytes but only 4 bytes of payload follow.
        let buf = vec![0, 0, 0, 100, 1, 2, 3, 4];
        let result = decode_message::<GuestRequest>(&buf);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // SetEnv request/response round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn test_request_set_env_roundtrip() {
        let mut vars = HashMap::new();
        vars.insert("ANTHROPIC_API_KEY".into(), "sk-ant-test123".into());
        vars.insert("MY_VAR".into(), "hello".into());
        let req = GuestRequest::SetEnv(SetEnvRequest { vars: vars.clone() });
        let rt = roundtrip_request(&req);
        if let GuestRequest::SetEnv(se) = rt {
            assert_eq!(se.vars.len(), 2);
            assert_eq!(se.vars.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant-test123");
            assert_eq!(se.vars.get("MY_VAR").unwrap(), "hello");
        } else {
            panic!("expected SetEnv variant");
        }
    }

    #[test]
    fn test_request_set_env_empty_roundtrip() {
        let req = GuestRequest::SetEnv(SetEnvRequest {
            vars: HashMap::new(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::SetEnv(se) = rt {
            assert!(se.vars.is_empty());
        } else {
            panic!("expected SetEnv variant");
        }
    }

    #[test]
    fn test_response_set_env_result_roundtrip() {
        let resp = GuestResponse::SetEnvResult(SetEnvResponse { count: 3 });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::SetEnvResult(r) = rt {
            assert_eq!(r.count, 3);
        } else {
            panic!("expected SetEnvResult variant");
        }
    }

    #[test]
    fn test_set_env_json_format() {
        // Verify the JSON wire format matches expectations
        let req = GuestRequest::SetEnv(SetEnvRequest {
            vars: HashMap::from([("KEY".into(), "val".into())]),
        });
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""type":"SetEnv""#));
        assert!(json.contains(r#""KEY":"val""#));
    }

    // -----------------------------------------------------------------------
    // Vault protocol round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn test_vault_read_roundtrip() {
        let req = GuestRequest::VaultRead(VaultReadRequest {
            path: "teams/alpha/notes/status.md".to_string(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::VaultRead(vr) = rt {
            assert_eq!(vr.path, "teams/alpha/notes/status.md");
        } else {
            panic!("expected VaultRead variant");
        }
    }

    #[test]
    fn test_vault_write_roundtrip() {
        let req = GuestRequest::VaultWrite(VaultWriteRequest {
            path: "teams/alpha/tasks/task-1.md".to_string(),
            content: "# Task 1\nDo something.".to_string(),
            mode: "overwrite".to_string(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::VaultWrite(vw) = rt {
            assert_eq!(vw.path, "teams/alpha/tasks/task-1.md");
            assert_eq!(vw.mode, "overwrite");
        } else {
            panic!("expected VaultWrite variant");
        }
    }

    #[test]
    fn test_vault_write_default_mode() {
        let json = r#"{"type":"VaultWrite","path":"test.md","content":"hello"}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::VaultWrite(vw) = req {
            assert_eq!(vw.mode, "overwrite");
        } else {
            panic!("expected VaultWrite variant");
        }
    }

    #[test]
    fn test_vault_search_roundtrip() {
        let req = GuestRequest::VaultSearch(VaultSearchRequest {
            query: "TODO".to_string(),
            max_results: 10,
            tag_filter: Some("urgent".to_string()),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::VaultSearch(vs) = rt {
            assert_eq!(vs.query, "TODO");
            assert_eq!(vs.max_results, 10);
            assert_eq!(vs.tag_filter.as_deref(), Some("urgent"));
        } else {
            panic!("expected VaultSearch variant");
        }
    }

    #[test]
    fn test_vault_search_default_max_results() {
        let json = r#"{"type":"VaultSearch","query":"test"}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::VaultSearch(vs) = req {
            assert_eq!(vs.max_results, 20);
        } else {
            panic!("expected VaultSearch variant");
        }
    }

    #[test]
    fn test_vault_list_roundtrip() {
        let req = GuestRequest::VaultList(VaultListRequest {
            path: Some("teams/alpha".to_string()),
            recursive: true,
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::VaultList(vl) = rt {
            assert_eq!(vl.path.as_deref(), Some("teams/alpha"));
            assert!(vl.recursive);
        } else {
            panic!("expected VaultList variant");
        }
    }

    #[test]
    fn test_vault_content_response_roundtrip() {
        let mut fm = HashMap::new();
        fm.insert("status".to_string(), serde_json::Value::String("active".to_string()));
        let resp = GuestResponse::VaultContent(VaultContentResponse {
            path: "teams/alpha/notes/status.md".to_string(),
            content: "# Status\nAll good.".to_string(),
            frontmatter: Some(fm),
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::VaultContent(vc) = rt {
            assert_eq!(vc.path, "teams/alpha/notes/status.md");
            assert!(vc.frontmatter.is_some());
        } else {
            panic!("expected VaultContent variant");
        }
    }

    #[test]
    fn test_vault_write_ok_roundtrip() {
        let resp = GuestResponse::VaultWriteOk;
        let rt = roundtrip_response(&resp);
        assert!(matches!(rt, GuestResponse::VaultWriteOk));
    }

    #[test]
    fn test_vault_search_results_roundtrip() {
        let resp = GuestResponse::VaultSearchResults(VaultSearchResponse {
            results: vec![VaultSearchResultEntry {
                path: "notes/todo.md".to_string(),
                line_number: 5,
                line: "TODO: fix this".to_string(),
                context_before: vec!["line 4".to_string()],
                context_after: vec!["line 6".to_string()],
            }],
            total_matches: 1,
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::VaultSearchResults(vs) = rt {
            assert_eq!(vs.total_matches, 1);
            assert_eq!(vs.results.len(), 1);
        } else {
            panic!("expected VaultSearchResults variant");
        }
    }

    #[test]
    fn test_vault_entries_roundtrip() {
        let resp = GuestResponse::VaultEntries(VaultListResponse {
            entries: vec!["notes/".to_string(), "tasks/".to_string()],
        });
        let rt = roundtrip_response(&resp);
        if let GuestResponse::VaultEntries(ve) = rt {
            assert_eq!(ve.entries.len(), 2);
        } else {
            panic!("expected VaultEntries variant");
        }
    }
}
