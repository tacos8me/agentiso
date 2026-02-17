use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vsock port the guest agent listens on. Used by the host-side vsock client.
#[allow(dead_code)]
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

    /// Graceful shutdown request.
    Shutdown,
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
    30
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
    vec!["1.1.1.1".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetHostnameRequest {
    pub hostname: String,
}

// ---------------------------------------------------------------------------
// Guest -> Host responses
// ---------------------------------------------------------------------------

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
#[allow(dead_code)]
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
            timeout_secs: 30,
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
        // When timeout_secs is not in JSON, it should default to 30.
        let json = r#"{"type":"Exec","command":"ls","args":[],"env":{}}"#;
        let req: GuestRequest = serde_json::from_str(json).unwrap();
        if let GuestRequest::Exec(e) = req {
            assert_eq!(e.timeout_secs, 30);
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
}
