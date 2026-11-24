//! The Claude Messages API request behind summaries.
//!
//! Raw HTTP, because there is no official Rust SDK. The request streams: a long
//! summary takes minutes on a model that thinks first, and a non-streaming
//! request of that length runs into HTTP timeouts. Failures are typed so the
//! summary service can tell a lasting problem (rejected key, billing, spend
//! limit, outage after retries) from one worth reporting as it is, and hand a
//! lasting one to the local model.

use reqwest::{header::HeaderMap, Client, StatusCode};
use serde::Deserialize;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub const CLAUDE_API_BASE: &str = "https://api.anthropic.com";
pub const CLAUDE_API_VERSION: &str = "2026-10-21";
/// The model the app selects when the user has not chosen one.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-5-5";

/// Safety classifiers can decline a request (HTTP 200, `stop_reason: "refusal"`).
/// With this beta and `fallbacks: "default"` the API re-runs a declined request
/// on the model Anthropic recommends for that case, inside the same call.
const REFUSAL_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Attempts per request, counting the first. Only transient failures repeat.
const MAX_ATTEMPTS: u32 = 3;
/// The longest `retry-after` that is honoured before giving up on Claude.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeFailureKind {
    /// 401: missing, malformed or revoked key.
    Authentication,
    /// 402.
    Billing,
    /// 403.
    Permission,
    /// 404: unknown model, or one this account cannot use.
    ModelNotFound,
    /// 429 with `enforced_spend_limit_reached`: retries fail until access resumes.
    SpendLimit,
    /// 429, still limited after the retries.
    RateLimited,
    /// 5xx or `overloaded_error`, still failing after the retries.
    Unavailable,
    /// No response: DNS, TLS, connection reset, timeout, stream cut off.
    Network,
    /// 400/413/422: the request itself is wrong; another attempt cannot help.
    InvalidRequest,
    /// `stop_reason: "refusal"` after the server-side fallback, if any.
    Refused,
    /// `stop_reason: "max_tokens"`.
    Truncated,
    Cancelled,
    /// A response this code does not understand.
    Protocol,
}

impl ClaudeFailureKind {
    fn is_transient(self) -> bool {
        matches!(self, Self::RateLimited | Self::Unavailable | Self::Network)
    }

    /// True when the summary cannot come from Claude now, whatever is retried,
    /// while the transcript itself is fine: the local model can take over.
    pub fn is_lasting(self) -> bool {
        matches!(
            self,
            Self::Authentication
                | Self::Billing
                | Self::Permission
                | Self::ModelNotFound
                | Self::SpendLimit
                | Self::RateLimited
                | Self::Unavailable
                | Self::Network
                | Self::Refused
        )
    }

    /// Why Claude was not used, as a clause after "because".
    pub fn short_reason(self) -> &'static str {
        match self {
            Self::Authentication => "Claude rejected the API key",
            Self::Billing => "Claude reported a billing problem",
            Self::Permission => "the Claude API key is not allowed to make this request",
            Self::ModelNotFound => "the selected Claude model is not available to this account",
            Self::SpendLimit => "the Claude account reached its monthly spend limit",
            Self::RateLimited => "Claude kept rate limiting the request",
            Self::Unavailable => "Claude was unavailable",
            Self::Network => "Claude could not be reached",
            Self::InvalidRequest => "Claude rejected the request",
            Self::Refused => "Claude declined the request",
            Self::Truncated => "Claude reached the output limit",
            Self::Cancelled => "the request was cancelled",
            Self::Protocol => "Claude sent an unexpected response",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeFailure {
    pub kind: ClaudeFailureKind,
    pub message: String,
    retry_after: Option<Duration>,
}

impl ClaudeFailure {
    pub fn new(kind: ClaudeFailureKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), retry_after: None }
    }

    fn cancelled() -> Self {
        // The summary service recognises cancellation by this wording.
        Self::new(ClaudeFailureKind::Cancelled, "Summary generation was cancelled")
    }
}

impl std::fmt::Display for ClaudeFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

tokio::task_local! {
    /// The kind of the last failed Claude request in this task. The summary
    /// pipeline passes errors along as text and may wrap them; this keeps the
    /// classification intact for the service that decides on the local model.
    /// Outside a scope (other callers of the LLM client) nothing is recorded.
    pub static LAST_CLAUDE_FAILURE: Cell<Option<ClaudeFailureKind>>;
}

fn record(failure: &ClaudeFailure) {
    let _ = LAST_CLAUDE_FAILURE.try_with(|cell| cell.set(Some(failure.kind)));
}

/// Waits between attempts and for stream data. Tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct ClaudeTimings {
    /// First backoff; doubles per attempt. A `retry-after` header takes precedence.
    pub retry_base: Duration,
    /// The stream carries pings while the model thinks, so silence this long
    /// means the connection is dead.
    pub stream_idle: Duration,
    /// Upper bound for one attempt, connection to last byte.
    pub request: Duration,
}

impl Default for ClaudeTimings {
    fn default() -> Self {
        Self {
            retry_base: Duration::from_secs(2),
            stream_idle: Duration::from_secs(180),
            request: Duration::from_secs(30 * 60),
        }
    }
}

pub struct ClaudeCall<'a> {
    pub base_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    pub system_prompt: &'a str,
    pub user_prompt: &'a str,
    pub max_tokens: u32,
    pub timings: ClaudeTimings,
}

/// Sends the request, repeating transient failures, and returns the complete text.
pub async fn send_message(
    client: &Client,
    call: &ClaudeCall<'_>,
    cancellation: Option<&CancellationToken>,
) -> Result<String, ClaudeFailure> {
    let result = send_with_retries(client, call, cancellation).await;
    if let Err(failure) = &result {
        record(failure);
    }
    result
}

async fn send_with_retries(
    client: &Client,
    call: &ClaudeCall<'_>,
    cancellation: Option<&CancellationToken>,
) -> Result<String, ClaudeFailure> {
    let mut refusal_fallback = true;
    let mut attempt = 0;
    loop {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Err(ClaudeFailure::cancelled());
        }
        attempt += 1;
        let failure = match send_once(client, call, refusal_fallback, cancellation).await {
            Ok(text) => return Ok(text),
            Err(failure) => failure,
        };

        // Not every model or organization accepts the fallback beta. The request
        // is valid without it, so it is sent again once in its plain form.
        if refusal_fallback
            && failure.kind == ClaudeFailureKind::InvalidRequest
            && mentions_refusal_fallback(&failure.message)
        {
            info!("Claude did not accept the refusal fallback for {}; sending without it", call.model);
            refusal_fallback = false;
            attempt -= 1;
            continue;
        }

        if !failure.kind.is_transient() || attempt >= MAX_ATTEMPTS {
            return Err(failure);
        }
        let wait = failure
            .retry_after
            .unwrap_or_else(|| call.timings.retry_base * 2u32.pow(attempt - 1));
        if wait > MAX_RETRY_WAIT {
            return Err(failure);
        }
        warn!("Claude request failed ({:?}); attempt {} of {} follows in {:?}", failure.kind, attempt + 1, MAX_ATTEMPTS, wait);
        match cancellation {
            Some(token) => tokio::select! {
                _ = token.cancelled() => return Err(ClaudeFailure::cancelled()),
                _ = tokio::time::sleep(wait) => {}
            },
            None => tokio::time::sleep(wait).await,
        }
    }
}

fn mentions_refusal_fallback(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("fallbacks") || message.contains("anthropic-beta") || message.contains(REFUSAL_FALLBACK_BETA)
}

fn request_body(call: &ClaudeCall<'_>, refusal_fallback: bool) -> serde_json::Value {
    // No sampling parameters and no `thinking` field: current models reject the
    // former and choose the latter themselves.
    let mut body = serde_json::json!({
        "model": call.model,
        "max_tokens": call.max_tokens,
        "stream": true,
        "system": call.system_prompt,
        "messages": [{ "role": "user", "content": call.user_prompt }],
    });
    if refusal_fallback {
        body["fallbacks"] = serde_json::json!("default");
    }
    body
}

async fn send_once(
    client: &Client,
    call: &ClaudeCall<'_>,
    refusal_fallback: bool,
    cancellation: Option<&CancellationToken>,
) -> Result<String, ClaudeFailure> {
    let url = format!("{}/v1/messages", call.base_url.trim_end_matches('/'));
    let mut request = client
        .post(url)
        .header("x-api-key", call.api_key)
        .header("anthropic-version", CLAUDE_API_VERSION)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .timeout(call.timings.request)
        .json(&request_body(call, refusal_fallback));
    if refusal_fallback {
        request = request.header("anthropic-beta", REFUSAL_FALLBACK_BETA);
    }

    let response = cancellable(request.send(), cancellation)
        .await?
        .map_err(network_failure)?;

    let status = response.status();
    if !status.is_success() {
        let retry_after = retry_after(response.headers());
        let body = cancellable(response.text(), cancellation).await?.unwrap_or_default();
        let mut failure = failure_from_http(status, &body, call.model);
        failure.retry_after = retry_after;
        return Err(failure);
    }
    read_stream(response, call, cancellation).await
}

async fn cancellable<T>(
    future: impl std::future::Future<Output = T>,
    cancellation: Option<&CancellationToken>,
) -> Result<T, ClaudeFailure> {
    match cancellation {
        Some(token) => tokio::select! {
            biased;
            _ = token.cancelled() => Err(ClaudeFailure::cancelled()),
            value = future => Ok(value),
        },
        None => Ok(future.await),
    }
}

fn network_failure(error: reqwest::Error) -> ClaudeFailure {
    let detail = if error.is_timeout() {
        "the request timed out".to_string()
    } else if error.is_connect() {
        "no connection could be made".to_string()
    } else {
        // reqwest errors can carry the URL; none of it is secret, but keep it short.
        error.without_url().to_string()
    };
    ClaudeFailure::new(ClaudeFailureKind::Network, format!("Claude could not be reached: {detail}."))
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers.get("retry-after")?.to_str().ok()?.trim().parse::<f64>().ok()?;
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

#[derive(Deserialize, Default)]
struct ApiErrorEnvelope {
    #[serde(default)]
    error: ApiError,
}

#[derive(Deserialize, Default)]
struct ApiError {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    details: Option<serde_json::Value>,
}

fn failure_from_http(status: StatusCode, body: &str, model: &str) -> ClaudeFailure {
    let error = serde_json::from_str::<ApiErrorEnvelope>(body).map(|envelope| envelope.error).unwrap_or_default();
    failure_from_api_error(Some(status), &error, model)
}

fn failure_from_api_error(status: Option<StatusCode>, error: &ApiError, model: &str) -> ClaudeFailure {
    use ClaudeFailureKind as Kind;
    let code = status.map(|status| status.as_u16());
    let spend_limit = error
        .details
        .as_ref()
        .and_then(|details| details.get("error_code"))
        .and_then(|value| value.as_str())
        == Some("enforced_spend_limit_reached");
    let kind = match (code, error.kind.as_str()) {
        (Some(401), _) | (_, "authentication_error") => Kind::Authentication,
        (Some(402), _) | (_, "billing_error") => Kind::Billing,
        (Some(403), _) | (_, "permission_error") => Kind::Permission,
        (Some(404), _) | (_, "not_found_error") => Kind::ModelNotFound,
        (Some(429), _) | (_, "rate_limit_error") if spend_limit => Kind::SpendLimit,
        (Some(429), _) | (_, "rate_limit_error") => Kind::RateLimited,
        (Some(500..=599), _) | (_, "api_error" | "overloaded_error") => Kind::Unavailable,
        (Some(400 | 413 | 422), _) | (_, "invalid_request_error" | "request_too_large") => Kind::InvalidRequest,
        _ => Kind::Protocol,
    };
    let said = error.message.trim();
    let quoted = if said.is_empty() { String::new() } else { format!(" Claude said: {said}") };
    let message = match kind {
        Kind::Authentication => format!("Claude rejected the API key. Check the key in Settings.{quoted}"),
        Kind::Billing => format!("Claude reported a billing problem with this account.{quoted}"),
        Kind::Permission => format!("This Claude API key is not allowed to make this request.{quoted}"),
        Kind::ModelNotFound => format!("Claude does not offer the model {model} to this account.{quoted}"),
        Kind::SpendLimit => format!("This Claude account reached its monthly spend limit.{quoted}"),
        Kind::RateLimited => format!("Claude is rate limiting this account.{quoted}"),
        Kind::Unavailable => format!("Claude is temporarily unavailable.{quoted}"),
        Kind::InvalidRequest => format!("Claude rejected the request.{quoted}"),
        _ => match code {
            Some(code) => format!("Claude answered with HTTP {code}.{quoted}"),
            None => format!("Claude reported an error.{quoted}"),
        },
    };
    ClaudeFailure::new(kind, message)
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEvent {
    ContentBlockStart { index: usize, content_block: StartedBlock },
    ContentBlockDelta { index: usize, delta: BlockDelta },
    MessageDelta { delta: MessageDelta },
    MessageStop,
    Error { error: ApiError },
    /// message_start, content_block_stop, ping, and anything added later.
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StartedBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    /// Marks the point where a declined request continued on another model.
    Fallback,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDelta {
    TextDelta { text: String },
    /// Thinking, signatures and tool input are never part of a summary.
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct MessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_details: Option<serde_json::Value>,
}

enum Block {
    Text(String),
    Fallback,
    Other,
}

#[derive(Default)]
struct StreamState {
    blocks: BTreeMap<usize, Block>,
    stop_reason: Option<String>,
    stop_details: Option<serde_json::Value>,
    finished: bool,
}

impl StreamState {
    fn apply(&mut self, event: StreamEvent, model: &str) -> Result<(), ClaudeFailure> {
        match event {
            StreamEvent::ContentBlockStart { index, content_block } => {
                self.blocks.insert(index, match content_block {
                    StartedBlock::Text { text } => Block::Text(text),
                    StartedBlock::Fallback => Block::Fallback,
                    StartedBlock::Other => Block::Other,
                });
            }
            StreamEvent::ContentBlockDelta { index, delta } => {
                if let (Some(Block::Text(text)), BlockDelta::TextDelta { text: more }) = (self.blocks.get_mut(&index), delta) {
                    text.push_str(&more);
                }
            }
            StreamEvent::MessageDelta { delta } => {
                if delta.stop_reason.is_some() {
                    self.stop_reason = delta.stop_reason;
                    self.stop_details = delta.stop_details;
                }
            }
            StreamEvent::MessageStop => self.finished = true,
            StreamEvent::Error { error } => return Err(failure_from_api_error(None, &error, model)),
            StreamEvent::Other => {}
        }
        Ok(())
    }

    /// Text blocks in order. A block that follows a fallback boundary continues
    /// the text before it, so it is joined without a line break.
    fn text(&self) -> String {
        let mut text = String::new();
        let mut continues = false;
        for block in self.blocks.values() {
            match block {
                Block::Text(part) => {
                    if !text.is_empty() && !continues {
                        text.push('\n');
                    }
                    text.push_str(part);
                    continues = false;
                }
                Block::Fallback => continues = true,
                Block::Other => {}
            }
        }
        text.trim().to_string()
    }

    fn finish(self) -> Result<String, ClaudeFailure> {
        use ClaudeFailureKind as Kind;
        match self.stop_reason.as_deref() {
            Some("end_turn" | "stop_sequence") => {}
            Some("max_tokens") => return Err(ClaudeFailure::new(Kind::Truncated, "Claude reached the output limit before completing the summary. Increase Maximum summary length in Model Settings or request a shorter summary, then retry. The incomplete result was not saved.")),
            Some("refusal") => {
                // Partial output of a declined request is discarded, not saved as a summary.
                let category = self.stop_details.as_ref()
                    .and_then(|details| details.get("category"))
                    .and_then(|value| value.as_str())
                    .map(|category| format!(" (category: {category})"))
                    .unwrap_or_default();
                return Err(ClaudeFailure::new(Kind::Refused, format!("Claude declined to write this summary{category}.")));
            }
            Some(reason) => return Err(ClaudeFailure::new(Kind::Protocol, format!("Claude did not complete the response (stop reason: {reason}). Please retry."))),
            // The connection closed before the message ended.
            None => return Err(ClaudeFailure::new(Kind::Network, "Claude could not be reached: the response was cut off before it was complete.")),
        }
        let text = self.text();
        if text.is_empty() {
            return Err(ClaudeFailure::new(Kind::Protocol, "No text in Claude response"));
        }
        Ok(text)
    }
}

async fn read_stream(
    mut response: reqwest::Response,
    call: &ClaudeCall<'_>,
    cancellation: Option<&CancellationToken>,
) -> Result<String, ClaudeFailure> {
    let mut state = StreamState::default();
    let mut buffer: Vec<u8> = Vec::new();
    while !state.finished {
        let next = tokio::time::timeout(call.timings.stream_idle, response.chunk());
        let chunk = match cancellable(next, cancellation).await? {
            Err(_) => return Err(ClaudeFailure::new(ClaudeFailureKind::Network, "Claude could not be reached: the response stopped arriving.")),
            Ok(Err(error)) => return Err(network_failure(error)),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => chunk,
        };
        buffer.extend_from_slice(&chunk);
        // Events end with a blank line. Bytes are split there, before decoding,
        // so a multi-byte character cut by a chunk boundary stays whole.
        while let Some((end, separator)) = event_boundary(&buffer) {
            let event: Vec<u8> = buffer.drain(..end + separator).take(end).collect();
            if let Some(parsed) = parse_event(&event) {
                state.apply(parsed, call.model)?;
            }
        }
    }
    state.finish()
}

fn event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let unix = buffer.windows(2).position(|window| window == b"\n\n").map(|at| (at, 2));
    let windows = buffer.windows(4).position(|window| window == b"\r\n\r\n").map(|at| (at, 4));
    match (unix, windows) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn parse_event(event: &[u8]) -> Option<StreamEvent> {
    let text = String::from_utf8_lossy(event);
    let data: String = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() {
        return None;
    }
    // An event this code cannot read is skipped; the stop reason still decides
    // whether the message is complete.
    serde_json::from_str(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One scripted reply per connection, in order; the last one repeats.
    #[derive(Clone)]
    enum Reply {
        Http(u16, Vec<(&'static str, String)>, String),
        /// Sends the bytes, then closes without finishing the message.
        Raw(String),
    }

    fn sse(events: &[serde_json::Value]) -> String {
        events.iter().map(|event| format!("event: {}\ndata: {}\n\n", event["type"].as_str().unwrap(), event)).collect()
    }

    fn ok_stream(events: &[serde_json::Value]) -> Reply {
        Reply::Http(200, vec![("content-type", "text/event-stream".into())], sse(events))
    }

    fn error_reply(status: u16, kind: &str, message: &str) -> Reply {
        Reply::Http(status, vec![], serde_json::json!({ "type": "error", "error": { "type": kind, "message": message } }).to_string())
    }

    fn summary_events(text: &str) -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({ "type": "message_start", "message": { "id": "msg_1", "model": "claude-opus-5-5" } }),
            serde_json::json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            serde_json::json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "private reasoning" } }),
            serde_json::json!({ "type": "content_block_stop", "index": 0 }),
            serde_json::json!({ "type": "ping" }),
            serde_json::json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            serde_json::json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": text } }),
            serde_json::json!({ "type": "content_block_stop", "index": 1 }),
            serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 12 } }),
            serde_json::json!({ "type": "message_stop" }),
        ]
    }

    struct FakeClaude {
        base_url: String,
        requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    impl FakeClaude {
        /// A loopback HTTP server. Each connection gets the next scripted reply.
        async fn start(replies: Vec<Reply>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            tokio::spawn(async move {
                let mut served = 0usize;
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else { return };
                    let reply = replies[served.min(replies.len() - 1)].clone();
                    served += 1;
                    let seen = seen.clone();
                    tokio::spawn(async move {
                        let mut received = Vec::new();
                        let mut chunk = [0u8; 8192];
                        let (head, body) = loop {
                            let read = socket.read(&mut chunk).await.unwrap_or(0);
                            if read == 0 { return; }
                            received.extend_from_slice(&chunk[..read]);
                            let Some(split) = received.windows(4).position(|window| window == b"\r\n\r\n") else { continue };
                            let head = String::from_utf8_lossy(&received[..split]).to_lowercase();
                            let length = head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|value| value.trim().parse::<usize>().ok()).unwrap_or(0);
                            if received.len() >= split + 4 + length {
                                break (head, received[split + 4..split + 4 + length].to_vec());
                            }
                        };
                        seen.lock().unwrap().push((head, serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null)));
                        let bytes = match reply {
                            Reply::Http(status, headers, body) => {
                                let mut head = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
                                for (name, value) in headers { head.push_str(&format!("{name}: {value}\r\n")); }
                                format!("{head}\r\n{body}")
                            }
                            // No content-length: the body ends when the connection closes.
                            Reply::Raw(body) => format!("HTTP/1.1 200 X\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"),
                        };
                        let _ = socket.write_all(bytes.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self { base_url, requests }
        }

        fn requests(&self) -> Vec<(String, serde_json::Value)> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn client() -> Client {
        // The container routes outbound HTTP through a proxy; loopback must bypass it.
        Client::builder().no_proxy().build().unwrap()
    }

    fn fast() -> ClaudeTimings {
        ClaudeTimings { retry_base: Duration::from_millis(5), stream_idle: Duration::from_millis(400), request: Duration::from_secs(10) }
    }

    async fn ask(server: &FakeClaude, model: &str) -> Result<String, ClaudeFailure> {
        let call = ClaudeCall {
            base_url: &server.base_url, api_key: "test-key", model,
            system_prompt: "Summarise.", user_prompt: "[00:01] You: hello", max_tokens: 32000, timings: fast(),
        };
        send_message(&client(), &call, None).await
    }

    #[tokio::test]
    async fn streams_the_summary_and_sends_a_current_request() {
        let server = FakeClaude::start(vec![ok_stream(&summary_events("# Summary\nAgreed on the budget."))]).await;
        assert_eq!(ask(&server, DEFAULT_CLAUDE_MODEL).await.unwrap(), "# Summary\nAgreed on the budget.");

        let (head, body) = server.requests().remove(0);
        assert!(head.contains("x-api-key: test-key"));
        assert!(head.contains("anthropic-version: 2026-10-21"));
        assert!(head.contains("anthropic-beta: server-side-fallback-2026-07-01"));
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 32000);
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["system"], "Summarise.");
        assert_eq!(body["messages"][0]["role"], "user");
        // Current models reject sampling parameters and manual thinking budgets.
        for absent in ["temperature", "top_p", "top_k", "thinking"] {
            assert!(body.get(absent).is_none(), "{absent} must not be sent");
        }
    }

    #[tokio::test]
    async fn text_split_across_chunks_and_multibyte_characters_survives() {
        let events = sse(&summary_events("Café déjà vu — 会議の要約"));
        // Byte-by-byte delivery cuts every multi-byte character and every event.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut chunk = [0u8; 8192];
            let _ = socket.read(&mut chunk).await;
            socket.write_all(b"HTTP/1.1 200 X\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n").await.unwrap();
            for byte in events.as_bytes() {
                socket.write_all(&[*byte]).await.unwrap();
                socket.flush().await.unwrap();
            }
            let _ = socket.shutdown().await;
        });
        let call = ClaudeCall { base_url: &base_url, api_key: "k", model: "claude-opus-5-5", system_prompt: "s", user_prompt: "u", max_tokens: 100, timings: fast() };
        assert_eq!(send_message(&client(), &call, None).await.unwrap(), "Café déjà vu — 会議の要約");
    }

    #[tokio::test]
    async fn a_request_declined_and_continued_on_another_model_reads_as_one_text() {
        let events = vec![
            serde_json::json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            serde_json::json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "The team agreed to" } }),
            serde_json::json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "fallback", "from": { "model": "claude-opus-5-5" }, "to": { "model": "claude-opus-5" } } }),
            serde_json::json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "text", "text": "" } }),
            serde_json::json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "text_delta", "text": " ship on Friday." } }),
            serde_json::json!({ "type": "content_block_start", "index": 3, "content_block": { "type": "text", "text": "Next: tests." } }),
            serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } }),
            serde_json::json!({ "type": "message_stop" }),
        ];
        let server = FakeClaude::start(vec![ok_stream(&events)]).await;
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap(), "The team agreed to ship on Friday.\nNext: tests.");
    }

    #[tokio::test]
    async fn incomplete_responses_never_become_a_summary() {
        let stopped = |reason: &str, details: serde_json::Value| ok_stream(&[
            serde_json::json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "A plausible but incomplete summary" } }),
            serde_json::json!({ "type": "message_delta", "delta": { "stop_reason": reason, "stop_details": details } }),
            serde_json::json!({ "type": "message_stop" }),
        ]);
        for (reason, kind) in [
            ("max_tokens", ClaudeFailureKind::Truncated),
            ("pause_turn", ClaudeFailureKind::Protocol),
            ("tool_use", ClaudeFailureKind::Protocol),
            ("something_new", ClaudeFailureKind::Protocol),
        ] {
            let server = FakeClaude::start(vec![stopped(reason, serde_json::Value::Null)]).await;
            assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap_err().kind, kind, "{reason}");
        }
        let server = FakeClaude::start(vec![stopped("refusal", serde_json::json!({ "category": "cyber" }))]).await;
        let refused = ask(&server, "claude-opus-5-5").await.unwrap_err();
        assert_eq!(refused.kind, ClaudeFailureKind::Refused);
        assert!(refused.message.contains("category: cyber"));
        // A refusal without details is a valid, permanent state.
        let server = FakeClaude::start(vec![stopped("refusal", serde_json::Value::Null)]).await;
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap_err().kind, ClaudeFailureKind::Refused);
    }

    #[tokio::test]
    async fn http_errors_are_classified_in_plain_words() {
        for (status, kind, said, expected, lasting) in [
            (401, "authentication_error", "invalid x-api-key", ClaudeFailureKind::Authentication, true),
            (402, "billing_error", "payment failed", ClaudeFailureKind::Billing, true),
            (403, "permission_error", "not allowed", ClaudeFailureKind::Permission, true),
            (404, "not_found_error", "model: claude-nope", ClaudeFailureKind::ModelNotFound, true),
            (400, "invalid_request_error", "prompt is too long", ClaudeFailureKind::InvalidRequest, false),
            (413, "request_too_large", "too large", ClaudeFailureKind::InvalidRequest, false),
        ] {
            let server = FakeClaude::start(vec![error_reply(status, kind, said)]).await;
            let failure = ask(&server, "claude-opus-5-5").await.unwrap_err();
            assert_eq!(failure.kind, expected, "{status}");
            assert_eq!(failure.kind.is_lasting(), lasting, "{status}");
            assert!(failure.message.contains(said), "{}", failure.message);
            // Not transient: exactly one request.
            assert_eq!(server.requests().len(), 1, "{status}");
        }
    }

    #[tokio::test]
    async fn a_spend_limit_is_not_retried_but_a_rate_limit_is() {
        let spend = Reply::Http(429, vec![], serde_json::json!({ "type": "error", "error": {
            "type": "rate_limit_error", "message": "spend limit", "details": { "error_code": "enforced_spend_limit_reached" } } }).to_string());
        let server = FakeClaude::start(vec![spend]).await;
        let failure = ask(&server, "claude-opus-5-5").await.unwrap_err();
        assert_eq!(failure.kind, ClaudeFailureKind::SpendLimit);
        assert_eq!(server.requests().len(), 1);

        let limited = Reply::Http(429, vec![("retry-after", "0.01".into())], serde_json::json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "slow down" } }).to_string());
        let server = FakeClaude::start(vec![limited.clone(), limited.clone(), ok_stream(&summary_events("Done."))]).await;
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap(), "Done.");
        assert_eq!(server.requests().len(), 3);

        // Still limited after every attempt: a lasting failure, for the local model.
        let server = FakeClaude::start(vec![limited]).await;
        let failure = ask(&server, "claude-opus-5-5").await.unwrap_err();
        assert_eq!(failure.kind, ClaudeFailureKind::RateLimited);
        assert!(failure.kind.is_lasting());
        assert_eq!(server.requests().len(), MAX_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn an_overload_is_retried_and_a_long_wait_is_not_sat_out() {
        let server = FakeClaude::start(vec![error_reply(529, "overloaded_error", "busy"), ok_stream(&summary_events("Done."))]).await;
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap(), "Done.");

        let patient = Reply::Http(429, vec![("retry-after", "3600".into())], serde_json::json!({ "type": "error", "error": { "type": "rate_limit_error", "message": "come back later" } }).to_string());
        let server = FakeClaude::start(vec![patient]).await;
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap_err().kind, ClaudeFailureKind::RateLimited);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_error_inside_the_stream_and_a_cut_connection_are_transient() {
        let overloaded = ok_stream(&[
            serde_json::json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "Partial" } }),
            serde_json::json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }),
        ]);
        let cut = Reply::Raw(sse(&[serde_json::json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "Partial" } })]));
        let server = FakeClaude::start(vec![overloaded, cut, ok_stream(&summary_events("Whole."))]).await;
        // Neither partial text is kept.
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap(), "Whole.");
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn a_stream_that_goes_silent_is_reported_as_unreachable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut chunk = [0u8; 8192];
                    let _ = socket.read(&mut chunk).await;
                    let _ = socket.write_all(b"HTTP/1.1 200 X\r\ncontent-type: text/event-stream\r\n\r\nevent: ping\ndata: {\"type\":\"ping\"}\n\n").await;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        let timings = ClaudeTimings { retry_base: Duration::from_millis(1), stream_idle: Duration::from_millis(60), request: Duration::from_secs(10) };
        let call = ClaudeCall { base_url: &base_url, api_key: "k", model: "claude-opus-5-5", system_prompt: "s", user_prompt: "u", max_tokens: 100, timings };
        let failure = send_message(&client(), &call, None).await.unwrap_err();
        assert_eq!(failure.kind, ClaudeFailureKind::Network);
        assert!(failure.kind.is_lasting());
    }

    #[tokio::test]
    async fn a_model_that_does_not_accept_the_refusal_fallback_gets_the_plain_request() {
        let server = FakeClaude::start(vec![
            error_reply(400, "invalid_request_error", "Unexpected value(s) `server-side-fallback-2026-07-01` for the `anthropic-beta` header."),
            ok_stream(&summary_events("Done.")),
        ]).await;
        assert_eq!(ask(&server, "claude-3-haiku-20240307").await.unwrap(), "Done.");
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(!requests[1].0.contains("anthropic-beta"));
        assert!(requests[1].1.get("fallbacks").is_none());
        // Another 400 is the request's own fault and is not masked by the resend.
        let server = FakeClaude::start(vec![error_reply(400, "invalid_request_error", "prompt is too long")]).await;
        assert_eq!(ask(&server, "claude-opus-5-5").await.unwrap_err().kind, ClaudeFailureKind::InvalidRequest);
        assert_eq!(server.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_network_failure_after_retries() {
        // Bind, then drop: nothing listens on the port.
        let port = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");
        let call = ClaudeCall { base_url: &base_url, api_key: "k", model: "claude-opus-5-5", system_prompt: "s", user_prompt: "u", max_tokens: 100, timings: fast() };
        let failure = send_message(&client(), &call, None).await.unwrap_err();
        assert_eq!(failure.kind, ClaudeFailureKind::Network);
        assert!(failure.message.starts_with("Claude could not be reached"));
    }

    #[tokio::test]
    async fn cancellation_stops_a_running_request_and_a_retry_wait() {
        let server = FakeClaude::start(vec![error_reply(529, "overloaded_error", "busy")]).await;
        let token = CancellationToken::new();
        let timings = ClaudeTimings { retry_base: Duration::from_secs(30), ..fast() };
        let call = ClaudeCall { base_url: &server.base_url, api_key: "k", model: "claude-opus-5-5", system_prompt: "s", user_prompt: "u", max_tokens: 100, timings };
        let cancel = token.clone();
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(80)).await; cancel.cancel(); });
        let started = std::time::Instant::now();
        let failure = send_message(&client(), &call, Some(&token)).await.unwrap_err();
        assert_eq!(failure.kind, ClaudeFailureKind::Cancelled);
        assert!(failure.message.contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn the_failure_kind_reaches_the_summary_service_through_wrapped_errors() {
        let server = FakeClaude::start(vec![error_reply(401, "authentication_error", "invalid x-api-key")]).await;
        let recorded = LAST_CLAUDE_FAILURE.scope(Cell::new(None), async {
            // The pipeline keeps only text and may add context around it.
            let wrapped = ask(&server, "claude-opus-5-5").await.map_err(|failure| format!("Chunk 1 failed: {failure}"));
            assert!(wrapped.is_err());
            LAST_CLAUDE_FAILURE.with(Cell::get)
        }).await;
        assert_eq!(recorded, Some(ClaudeFailureKind::Authentication));
        // Outside a scope nothing is recorded and nothing panics.
        assert!(ask(&server, "claude-opus-5-5").await.is_err());
    }
}
