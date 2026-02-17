use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tracing::{debug, trace};

use crate::guest::protocol::{
    self, ExecRequest, ExecResponse, FileContentResponse, FileDataResponse, FileDownloadRequest,
    FileReadRequest, FileUploadRequest, FileWriteRequest, GuestRequest, GuestResponse,
    NetworkConfig, SetHostnameRequest,
};
use crate::guest;

/// An async vsock stream backed by `AsyncFd<OwnedFd>` with raw read/write.
///
/// We cannot wrap vsock fds in `tokio::net::UnixStream` because mio's internal
/// bookkeeping expects `AF_UNIX` semantics (e.g. `getpeername` with `sockaddr_un`).
/// Instead we implement `AsyncRead`/`AsyncWrite` directly via `libc::read`/`libc::write`.
struct VsockStream {
    inner: tokio::io::unix::AsyncFd<OwnedFd>,
}

impl VsockStream {
    fn new(fd: OwnedFd) -> std::io::Result<Self> {
        let inner = tokio::io::unix::AsyncFd::new(fd)?;
        Ok(Self { inner })
    }
}

impl AsyncRead for VsockStream {
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

impl AsyncWrite for VsockStream {
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

/// Client for communicating with the in-VM guest agent over vsock.
///
/// The protocol uses length-prefixed (4-byte big-endian) JSON messages
/// with typed request/response enums defined in `crate::guest::protocol`.
#[allow(dead_code)] // cid and port retained for diagnostics and public accessors
pub struct VsockClient {
    stream: VsockStream,
    cid: u32,
    port: u32,
}

#[allow(dead_code)] // Public API consumed by WorkspaceManager
impl VsockClient {
    /// Connect to the guest agent on the given vsock CID and port.
    ///
    /// vsock connections on Linux use AF_VSOCK (address family 40) sockets.
    /// Since tokio doesn't natively support AF_VSOCK, we create a raw socket
    /// via libc, connect it, set it non-blocking, and wrap it in a
    /// `VsockStream` backed by `AsyncFd<OwnedFd>`.
    async fn connect(cid: u32, port: u32) -> Result<Self> {
        let fd =
            tokio::task::spawn_blocking(move || -> Result<OwnedFd> {
                create_vsock_connection(cid, port)
            })
            .await
            .context("vsock connect task panicked")??;

        let stream = VsockStream::new(fd)
            .context("failed to register vsock fd with tokio")?;

        Ok(Self { stream, cid, port })
    }

    /// Connect to the guest agent and wait for the readiness handshake.
    ///
    /// Retries the connection until the guest agent is reachable or the timeout
    /// expires. Once connected, sends a Ping and expects a Pong back.
    pub async fn connect_and_wait(cid: u32, port: u32, timeout: Duration) -> Result<Self> {
        let deadline = tokio::time::Instant::now() + timeout;
        let retry_delay = Duration::from_millis(200);
        let mut last_error = None;

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(last_error
                    .unwrap_or_else(|| anyhow::anyhow!("guest agent connect timed out"))
                    .context(format!(
                        "guest agent at CID {} port {} not ready within {:?}",
                        cid, port, timeout
                    )));
            }

            match Self::connect(cid, port).await {
                Ok(mut client) => {
                    // Send a Ping to verify the agent is responsive
                    match tokio::time::timeout(Duration::from_secs(5), client.ping()).await {
                        Ok(Ok(pong)) => {
                            debug!(
                                cid,
                                port,
                                version = %pong.version,
                                uptime = pong.uptime_secs,
                                "guest agent ready"
                            );
                            return Ok(client);
                        }
                        Ok(Err(e)) => {
                            trace!(cid, port, error = %e, "ping failed, retrying");
                            last_error = Some(e);
                        }
                        Err(_) => {
                            trace!(cid, port, "ping timed out, retrying");
                            last_error =
                                Some(anyhow::anyhow!("guest agent ping timed out"));
                        }
                    }
                }
                Err(e) => {
                    trace!(cid, port, error = %e, "vsock connect failed, retrying");
                    last_error = Some(e);
                }
            }

            tokio::time::sleep(retry_delay).await;
        }
    }

    /// Send a request to the guest agent and read the response.
    async fn request(&mut self, req: &GuestRequest) -> Result<GuestResponse> {
        let (mut read_half, mut write_half) = tokio::io::split(&mut self.stream);
        guest::write_message(&mut write_half, req)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let resp: GuestResponse = guest::read_message(&mut read_half)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(resp)
    }

    /// Send a request with a timeout.
    async fn request_with_timeout(
        &mut self,
        req: &GuestRequest,
        timeout: Duration,
    ) -> Result<GuestResponse> {
        tokio::time::timeout(timeout, self.request(req))
            .await
            .context("vsock request timed out")?
    }

    /// Unwrap a GuestResponse, converting Error variants to anyhow errors.
    fn unwrap_response(resp: GuestResponse, context: &str) -> Result<GuestResponse> {
        match &resp {
            GuestResponse::Error(e) => {
                bail!("{}: {:?}: {}", context, e.code, e.message);
            }
            _ => Ok(resp),
        }
    }

    /// Send a Ping and return the Pong response.
    pub async fn ping(&mut self) -> Result<protocol::PongResponse> {
        let resp = self.request(&GuestRequest::Ping).await?;
        match Self::unwrap_response(resp, "ping")? {
            GuestResponse::Pong(pong) => Ok(pong),
            other => bail!("unexpected response to Ping: {:?}", other),
        }
    }

    /// Execute a command in the guest VM.
    pub async fn exec(
        &mut self,
        command: &str,
        args: Vec<String>,
        workdir: Option<String>,
        env: HashMap<String, String>,
        timeout_secs: u64,
    ) -> Result<ExecResponse> {
        let req = GuestRequest::Exec(ExecRequest {
            command: command.to_string(),
            args,
            workdir,
            env,
            timeout_secs,
        });

        // Allow extra time beyond the guest timeout for network overhead
        let timeout = Duration::from_secs(timeout_secs + 5);
        let resp = self.request_with_timeout(&req, timeout).await?;

        match Self::unwrap_response(resp, "exec")? {
            GuestResponse::ExecResult(result) => Ok(result),
            other => bail!("unexpected response to Exec: {:?}", other),
        }
    }

    /// Read a file from the guest filesystem.
    pub async fn file_read(
        &mut self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<FileContentResponse> {
        let req = GuestRequest::FileRead(FileReadRequest {
            path: path.to_string(),
            offset,
            limit,
        });

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(30))
            .await?;

        match Self::unwrap_response(resp, "file_read")? {
            GuestResponse::FileContent(content) => Ok(content),
            other => bail!("unexpected response to FileRead: {:?}", other),
        }
    }

    /// Write a file to the guest filesystem.
    ///
    /// `content` should be base64-encoded file data.
    pub async fn file_write(
        &mut self,
        path: &str,
        content: &str,
        mode: Option<u32>,
    ) -> Result<()> {
        let req = GuestRequest::FileWrite(FileWriteRequest {
            path: path.to_string(),
            content: content.to_string(),
            mode,
        });

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(30))
            .await?;

        Self::unwrap_response(resp, "file_write")?;
        Ok(())
    }

    /// Upload a file to the guest (host -> guest transfer).
    ///
    /// `data` should be base64-encoded file data.
    pub async fn file_upload(
        &mut self,
        guest_path: &str,
        data: &str,
        mode: Option<u32>,
    ) -> Result<()> {
        let req = GuestRequest::FileUpload(FileUploadRequest {
            guest_path: guest_path.to_string(),
            data: data.to_string(),
            mode,
        });

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(60))
            .await?;

        Self::unwrap_response(resp, "file_upload")?;
        Ok(())
    }

    /// Download a file from the guest (guest -> host transfer).
    ///
    /// Returns the base64-encoded file data and size.
    pub async fn file_download(&mut self, guest_path: &str) -> Result<FileDataResponse> {
        let req = GuestRequest::FileDownload(FileDownloadRequest {
            guest_path: guest_path.to_string(),
        });

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(60))
            .await?;

        match Self::unwrap_response(resp, "file_download")? {
            GuestResponse::FileData(data) => Ok(data),
            other => bail!("unexpected response to FileDownload: {:?}", other),
        }
    }

    /// Configure guest networking (IP, gateway, DNS).
    pub async fn configure_network(&mut self, config: NetworkConfig) -> Result<()> {
        let req = GuestRequest::ConfigureNetwork(config);

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(10))
            .await?;

        Self::unwrap_response(resp, "configure_network")?;
        Ok(())
    }

    /// Set the guest hostname.
    pub async fn set_hostname(&mut self, hostname: &str) -> Result<()> {
        let req = GuestRequest::SetHostname(SetHostnameRequest {
            hostname: hostname.to_string(),
        });

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(5))
            .await?;

        Self::unwrap_response(resp, "set_hostname")?;
        Ok(())
    }

    /// Request graceful shutdown of the guest agent.
    pub async fn shutdown(&mut self) -> Result<()> {
        let req = GuestRequest::Shutdown;

        // Shutdown may not respond before the VM goes down, so use a short timeout
        // and treat timeout as success.
        match self
            .request_with_timeout(&req, Duration::from_secs(3))
            .await
        {
            Ok(resp) => {
                Self::unwrap_response(resp, "shutdown")?;
                Ok(())
            }
            Err(e) => {
                // Connection reset or timeout is expected during shutdown
                debug!(error = %e, "shutdown request ended (expected during VM shutdown)");
                Ok(())
            }
        }
    }

    /// Get the vsock CID of the connected guest.
    pub fn cid(&self) -> u32 {
        self.cid
    }

    /// Get the vsock port of the connected guest agent.
    pub fn port(&self) -> u32 {
        self.port
    }
}

/// Create a raw AF_VSOCK connection and return an `OwnedFd`.
///
/// AF_VSOCK = 40, uses `struct sockaddr_vm` for addressing by CID + port.
/// The returned fd is set to non-blocking mode for use with `AsyncFd`.
fn create_vsock_connection(cid: u32, port: u32) -> Result<OwnedFd> {
    use std::os::fd::FromRawFd;

    // AF_VSOCK = 40
    const AF_VSOCK: i32 = 40;

    let fd = unsafe { libc::socket(AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        bail!(
            "failed to create vsock socket: {}",
            std::io::Error::last_os_error()
        );
    }

    // sockaddr_vm layout per include/uapi/linux/vm_sockets.h
    #[repr(C)]
    struct SockaddrVm {
        svm_family: libc::sa_family_t,
        svm_reserved1: u16,
        svm_port: u32,
        svm_cid: u32,
        svm_flags: u8,
        svm_zero: [u8; 3],
    }

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as libc::sa_family_t,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: cid,
        svm_flags: 0,
        svm_zero: [0u8; 3],
    };

    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const SockaddrVm as *const libc::sockaddr,
            std::mem::size_of::<SockaddrVm>() as libc::socklen_t,
        )
    };

    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        bail!("vsock connect to CID {} port {} failed: {}", cid, port, err);
    }

    // Set non-blocking for tokio AsyncFd compatibility
    let ret = unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        bail!("failed to set vsock socket non-blocking: {}", err);
    }

    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest::protocol::{
        encode_message, ErrorCode, ErrorResponse, ExecResponse, PongResponse, MAX_MESSAGE_SIZE,
    };
    use crate::guest;

    // -----------------------------------------------------------------------
    // Protocol message encoding
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_ping_request() {
        let req = GuestRequest::Ping;
        let encoded = encode_message(&req).unwrap();

        // First 4 bytes are the length prefix (big-endian)
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        assert_eq!(len as usize, encoded.len() - 4);

        // The JSON payload should deserialize back
        let payload = &encoded[4..];
        let decoded: GuestRequest = serde_json::from_slice(payload).unwrap();
        match decoded {
            GuestRequest::Ping => {} // expected
            _ => panic!("expected Ping"),
        }
    }

    #[test]
    fn test_encode_exec_request() {
        let req = GuestRequest::Exec(ExecRequest {
            command: "ls".to_string(),
            args: vec!["-la".to_string()],
            workdir: Some("/tmp".to_string()),
            env: HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            timeout_secs: 30,
        });

        let encoded = encode_message(&req).unwrap();
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        let payload = &encoded[4..];
        assert_eq!(payload.len(), len as usize);

        let decoded: GuestRequest = serde_json::from_slice(payload).unwrap();
        match decoded {
            GuestRequest::Exec(exec) => {
                assert_eq!(exec.command, "ls");
                assert_eq!(exec.args, vec!["-la"]);
                assert_eq!(exec.workdir, Some("/tmp".to_string()));
                assert_eq!(exec.timeout_secs, 30);
            }
            _ => panic!("expected Exec"),
        }
    }

    #[test]
    fn test_encode_pong_response() {
        let resp = GuestResponse::Pong(PongResponse {
            version: "1.0.0".to_string(),
            uptime_secs: 42,
        });

        let encoded = encode_message(&resp).unwrap();
        let len = u32::from_be_bytes([encoded[0], encoded[1], encoded[2], encoded[3]]);
        let payload = &encoded[4..];
        assert_eq!(payload.len(), len as usize);

        let decoded: GuestResponse = serde_json::from_slice(payload).unwrap();
        match decoded {
            GuestResponse::Pong(pong) => {
                assert_eq!(pong.version, "1.0.0");
                assert_eq!(pong.uptime_secs, 42);
            }
            _ => panic!("expected Pong"),
        }
    }

    // -----------------------------------------------------------------------
    // Read/write message round-trip over in-memory stream
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_write_read_message_roundtrip() {
        let req = GuestRequest::Ping;

        // Create an in-memory pipe
        let (client, server) = tokio::io::duplex(1024);
        let (mut server_read, _server_write) = tokio::io::split(server);
        let (_client_read, mut client_write) = tokio::io::split(client);

        // Write the message
        guest::write_message(&mut client_write, &req)
            .await
            .unwrap();

        // Read it back
        let decoded: GuestRequest = guest::read_message(&mut server_read).await.unwrap();
        match decoded {
            GuestRequest::Ping => {} // expected
            _ => panic!("expected Ping"),
        }
    }

    #[tokio::test]
    async fn test_write_read_exec_response_roundtrip() {
        let resp = GuestResponse::ExecResult(ExecResponse {
            exit_code: 0,
            stdout: "hello world\n".to_string(),
            stderr: "".to_string(),
        });

        let (client, server) = tokio::io::duplex(4096);
        let (mut server_read, _server_write) = tokio::io::split(server);
        let (_client_read, mut client_write) = tokio::io::split(client);

        guest::write_message(&mut client_write, &resp)
            .await
            .unwrap();

        let decoded: GuestResponse = guest::read_message(&mut server_read).await.unwrap();
        match decoded {
            GuestResponse::ExecResult(result) => {
                assert_eq!(result.exit_code, 0);
                assert_eq!(result.stdout, "hello world\n");
                assert!(result.stderr.is_empty());
            }
            _ => panic!("expected ExecResult"),
        }
    }

    // -----------------------------------------------------------------------
    // unwrap_response helper
    // -----------------------------------------------------------------------

    #[test]
    fn test_unwrap_response_ok() {
        let resp = GuestResponse::Ok;
        let result = VsockClient::unwrap_response(resp, "test");
        assert!(result.is_ok());
    }

    #[test]
    fn test_unwrap_response_pong() {
        let resp = GuestResponse::Pong(PongResponse {
            version: "1.0".to_string(),
            uptime_secs: 10,
        });
        let result = VsockClient::unwrap_response(resp, "ping");
        assert!(result.is_ok());
    }

    #[test]
    fn test_unwrap_response_error() {
        let resp = GuestResponse::Error(ErrorResponse {
            code: ErrorCode::NotFound,
            message: "file not found".to_string(),
        });
        let result = VsockClient::unwrap_response(resp, "file_read");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("file_read"));
        assert!(err_msg.contains("file not found"));
    }

    #[test]
    fn test_unwrap_response_error_codes() {
        for code in [
            ErrorCode::PermissionDenied,
            ErrorCode::Timeout,
            ErrorCode::IoError,
            ErrorCode::InvalidRequest,
            ErrorCode::Internal,
        ] {
            let resp = GuestResponse::Error(ErrorResponse {
                code,
                message: "test error".to_string(),
            });
            assert!(VsockClient::unwrap_response(resp, "test").is_err());
        }
    }

    // -----------------------------------------------------------------------
    // Protocol constants
    // -----------------------------------------------------------------------

    #[test]
    fn test_max_message_size() {
        assert_eq!(MAX_MESSAGE_SIZE, 16 * 1024 * 1024);
    }

    #[test]
    fn test_guest_agent_port_constant() {
        assert_eq!(protocol::GUEST_AGENT_PORT, 5000);
    }
}
