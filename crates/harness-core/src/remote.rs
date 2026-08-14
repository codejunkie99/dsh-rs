use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use zeroize::Zeroize;

use crate::events::Usage;
use crate::llm::{ChatMessage, LlmAdapter, LlmRequest, NullAdapter, StreamFrame, ToolSchema};

const DEFAULT_DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com";
static MODEL_RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub struct ModelSelection {
    pub adapter: Arc<dyn LlmAdapter>,
    pub model: String,
    pub is_remote: bool,
}

impl ModelSelection {
    pub fn select(
        environment_key: Option<&str>,
        credential_path: &Path,
        model: Option<&str>,
    ) -> Result<Self> {
        let model = model
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or("deepseek-chat");

        match CredentialStore::resolve(environment_key, credential_path) {
            Ok(api_key) => {
                let adapter = OpenAiCompatibleAdapter::deepseek(api_key, Some(model.to_string()))?;
                Ok(Self {
                    adapter: Arc::new(adapter),
                    model: model.to_string(),
                    is_remote: true,
                })
            }
            Err(_) => Ok(Self {
                adapter: Arc::new(NullAdapter),
                model: "local-null".to_string(),
                is_remote: false,
            }),
        }
    }

    pub fn select_from_environment(home: Option<&Path>) -> Result<Self> {
        let credential_path = CredentialStore::credential_path(home);
        Self::select(
            std::env::var("DEEPSEEK_API_KEY").ok().as_deref(),
            &credential_path,
            std::env::var("DEEPSEEK_MODEL").ok().as_deref(),
        )
    }
}

#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey([redacted])")
    }
}

impl std::fmt::Display for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl Drop for ApiKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, Clone, Default)]
pub struct CredentialStore;

impl CredentialStore {
    pub fn resolve(environment_key: Option<&str>, file: &Path) -> Result<ApiKey> {
        if let Some(key) = environment_key.map(str::trim).filter(|key| !key.is_empty()) {
            return Ok(ApiKey::new(key));
        }

        if !file.exists() {
            bail!("DeepSeek API credential is not configured");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file)?.permissions().mode();
            if mode & 0o077 != 0 {
                bail!("credential file permissions are too broad; require 0600");
            }
        }

        let contents = std::fs::read_to_string(file)
            .with_context(|| format!("failed to read credential file {}", file.display()))?;
        let key = contents
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == "api_key").then(|| value.trim().trim_matches('"').to_string())
            })
            .filter(|key| !key.is_empty())
            .ok_or_else(|| anyhow!("credential file does not contain api_key"))?;
        Ok(ApiKey::new(key))
    }

    pub fn resolve_from_environment(home: Option<&Path>) -> Result<ApiKey> {
        let path = Self::credential_path(home);
        Self::resolve(std::env::var("DEEPSEEK_API_KEY").ok().as_deref(), &path)
    }

    pub fn credential_path(home: Option<&Path>) -> PathBuf {
        home.map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".dsh-rs")
            .join("credentials")
    }
}

#[derive(Clone)]
pub struct OpenAiCompatibleAdapter {
    endpoint: String,
    api_key: ApiKey,
    model: String,
    client: reqwest::Client,
}

impl OpenAiCompatibleAdapter {
    pub fn deepseek(api_key: ApiKey, model: Option<String>) -> Result<Self> {
        let endpoint = std::env::var("DEEPSEEK_BASE_URL")
            .ok()
            .and_then(|value| (!value.trim().is_empty()).then_some(value))
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_ENDPOINT.to_string());
        Self::new(
            endpoint,
            api_key,
            model.unwrap_or_else(|| "deepseek-chat".into()),
        )
    }

    pub fn new(endpoint: impl AsRef<str>, api_key: ApiKey, model: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("failed to construct model client")?;
        Ok(Self {
            endpoint: endpoint.as_ref().trim_end_matches('/').to_string(),
            api_key,
            model,
            client,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn request_body(request: &LlmRequest) -> Result<Value> {
        let messages = request
            .messages
            .iter()
            .map(|message| match message {
                ChatMessage::System { content } => json!({"role": "system", "content": content}),
                ChatMessage::User { content } => json!({"role": "user", "content": content}),
                ChatMessage::Assistant { content } => {
                    json!({"role": "assistant", "content": content})
                }
                ChatMessage::AssistantToolCall {
                    content,
                    tool_calls,
                } => {
                    let tool_calls = tool_calls
                        .iter()
                        .map(|tool| {
                            json!({
                                "id": tool.id,
                                "type": "function",
                                "function": {
                                    "name": tool.name,
                                    "arguments": tool.arguments.to_string(),
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({
                        "role": "assistant",
                        "content": content,
                        "tool_calls": tool_calls,
                    })
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } => json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": content,
                }),
            })
            .collect::<Vec<_>>();

        let tools = request.tools.iter().map(function_tool).collect::<Vec<_>>();

        let mut body = json!({
            "model": request.model.clone().unwrap_or_else(|| "deepseek-chat".into()),
            "messages": messages,
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        Ok(body)
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.endpoint)
    }
}

fn function_tool(tool: &ToolSchema) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        }
    })
}

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.as_u16() >= 500
}

fn retry_delay(response: &reqwest::Response, attempt: u32) -> Duration {
    let fallback = Duration::from_millis(50_u64.saturating_mul(1_u64 << attempt.min(5)));
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|seconds: &f64| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| Duration::from_secs_f64(seconds.clamp(0.0, 30.0)))
        .unwrap_or(fallback)
}

#[async_trait]
impl LlmAdapter for OpenAiCompatibleAdapter {
    async fn stream(&self, request: LlmRequest) -> Result<mpsc::Receiver<StreamFrame>> {
        let model = request.model.clone().unwrap_or_else(|| self.model.clone());
        let mut body = Self::request_body(&request)?;
        body["model"] = Value::String(model);

        let request = self
            .client
            .post(self.chat_completions_url())
            .bearer_auth(self.api_key.expose())
            .json(&body)
            .build()
            .context("failed to construct model request")?;

        let client = self.client.clone();
        let (tx, rx) = mpsc::channel(32);
        MODEL_RUNTIME
            .get_or_init(|| Runtime::new().expect("failed to start model runtime"))
            .spawn(async move {
                let mut response = None;
                for attempt in 0_u32..3 {
                    let executable = match request.try_clone() {
                        Some(request) => request,
                        None => {
                            let _ = tx
                                .send(StreamFrame::Error {
                                    message: "model request is not retryable".into(),
                                })
                                .await;
                            return;
                        }
                    };

                    match client.execute(executable).await {
                        Ok(candidate) => {
                            if candidate.status().is_success() {
                                response = Some(candidate);
                                break;
                            }

                            let status = candidate.status();
                            let delay = retry_delay(&candidate, attempt);
                            let detail = candidate
                                .text()
                                .await
                                .unwrap_or_else(|error| format!("unreadable error body: {error}"));
                            if !retryable_status(status) || attempt == 2 {
                                let _ = tx
                                    .send(StreamFrame::Error {
                                        message: format!(
                                            "model request failed with {status}: {detail}"
                                        ),
                                    })
                                    .await;
                                return;
                            }
                            tokio::time::sleep(delay).await;
                        }
                        Err(error) => {
                            if attempt == 2 {
                                let _ = tx
                                    .send(StreamFrame::Error {
                                        message: format!(
                                            "model request failed after retries: {error}"
                                        ),
                                    })
                                    .await;
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(
                                50_u64.saturating_mul(1_u64 << attempt.min(5)),
                            ))
                            .await;
                        }
                    }
                }

                let Some(response) = response else {
                    let _ = tx
                        .send(StreamFrame::Error {
                            message: "model request exhausted retries".into(),
                        })
                        .await;
                    return;
                };

                if !response.status().is_success() {
                    let status = response.status();
                    let detail = response
                        .text()
                        .await
                        .unwrap_or_else(|error| format!("unreadable error body: {error}"));
                    let _ = tx
                        .send(StreamFrame::Error {
                            message: format!("model request failed with {status}: {detail}"),
                        })
                        .await;
                    return;
                }

                let mut stream = response.bytes_stream();
                let mut decoder = SseDecoder::new();
                let mut buffer = Vec::new();
                let message_id = uuid::Uuid::new_v4();

                while let Some(chunk) = stream.next().await {
                    let chunk = match chunk {
                        Ok(chunk) => chunk,
                        Err(error) => {
                            let _ = tx
                                .send(StreamFrame::Error {
                                    message: format!("model stream failed: {error}"),
                                })
                                .await;
                            return;
                        }
                    };
                    buffer.extend_from_slice(&chunk);

                    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line = buffer.drain(..=position).collect::<Vec<_>>();
                        if let Some(data) = sse_data_line(&line) {
                            match decoder.ingest_data(&data) {
                                Ok(events) => {
                                    for event in events {
                                        if let Some(frame) = decoder_frame(event, message_id) {
                                            if tx.send(frame).await.is_err() {
                                                return;
                                            }
                                        }
                                    }
                                }
                                Err(error) => {
                                    let _ = tx
                                        .send(StreamFrame::Error {
                                            message: format!("invalid model stream event: {error}"),
                                        })
                                        .await;
                                    return;
                                }
                            }
                        }
                    }
                }

                match decoder.finish() {
                    Ok(events) => {
                        for event in events {
                            if let Some(frame) = decoder_frame(event, message_id) {
                                if tx.send(frame).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = tx
                            .send(StreamFrame::Error {
                                message: format!("incomplete model stream: {error}"),
                            })
                            .await;
                    }
                }
            });
        Ok(rx)
    }
}

fn sse_data_line(bytes: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(bytes);
    let line = line.trim_end_matches(['\r', '\n']);
    line.strip_prefix("data:")
        .map(|data| data.trim().to_string())
}

fn decoder_frame(event: DecoderEvent, message_id: uuid::Uuid) -> Option<StreamFrame> {
    match event {
        DecoderEvent::Delta(text) => Some(StreamFrame::Delta { message_id, text }),
        DecoderEvent::ToolCall {
            id,
            name,
            arguments,
        } => Some(StreamFrame::ToolCall {
            id,
            name,
            arguments,
        }),
        DecoderEvent::Done { stop_reason, usage } => Some(StreamFrame::Done { stop_reason, usage }),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecoderEvent {
    Delta(String),
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    Done {
        stop_reason: Option<String>,
        usage: Option<Usage>,
    },
}

#[derive(Default)]
pub struct SseDecoder {
    pending_tools: BTreeMap<usize, PendingToolCall>,
    stop_reason: Option<String>,
    usage: Option<Usage>,
    done: bool,
    emitted_done: bool,
}

#[derive(Default)]
struct PendingToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest_data(&mut self, data: &str) -> Result<Vec<DecoderEvent>> {
        if self.done {
            bail!("received data after SSE stream completion");
        }
        if data == "[DONE]" {
            self.done = true;
            return self.finish();
        }

        let chunk: CompletionChunk = serde_json::from_str(data)
            .with_context(|| format!("invalid chat completion JSON: {data}"))?;
        let mut events = Vec::new();

        for choice in chunk.choices {
            if let Some(content) = choice.delta.content {
                if !content.is_empty() {
                    events.push(DecoderEvent::Delta(content));
                }
            }
            for tool in choice.delta.tool_calls {
                let pending = self.pending_tools.entry(tool.index).or_default();
                if let Some(id) = tool.id {
                    pending.id = Some(id);
                }
                if let Some(function) = tool.function {
                    if let Some(name) = function.name {
                        pending.name = Some(name);
                    }
                    if let Some(arguments) = function.arguments {
                        pending.arguments.push_str(&arguments);
                    }
                }
            }
            if let Some(reason) = choice.finish_reason {
                self.stop_reason = Some(reason);
            }
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage.into());
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Result<Vec<DecoderEvent>> {
        if self.emitted_done {
            return Ok(Vec::new());
        }
        self.emitted_done = true;
        let mut events = Vec::new();
        let pending = std::mem::take(&mut self.pending_tools);
        for (index, tool) in pending {
            let arguments = if tool.arguments.trim().is_empty() {
                Value::Object(Default::default())
            } else {
                serde_json::from_str(&tool.arguments).with_context(|| {
                    format!("invalid accumulated arguments for tool call {index}")
                })?
            };
            events.push(DecoderEvent::ToolCall {
                id: tool.id.unwrap_or_else(|| format!("call_{index}")),
                name: tool.name.unwrap_or_else(|| "unknown".into()),
                arguments,
            });
        }
        events.push(DecoderEvent::Done {
            stop_reason: self.stop_reason.clone(),
            usage: self.usage.clone(),
        });
        Ok(events)
    }
}

#[derive(Debug, Deserialize)]
struct CompletionChunk {
    #[serde(default)]
    choices: Vec<CompletionChoice>,
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct CompletionChoice {
    delta: CompletionDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CompletionDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Deserialize)]
struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    prompt_cache_hit_tokens: Option<u64>,
    prompt_cache_miss_tokens: Option<u64>,
}

impl From<ApiUsage> for Usage {
    fn from(value: ApiUsage) -> Self {
        Usage {
            input_tokens: value.prompt_tokens,
            output_tokens: value.completion_tokens,
            cache_read_tokens: value.prompt_cache_hit_tokens,
            cache_write_tokens: value.prompt_cache_miss_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use crate::llm::{ChatMessage, LlmAdapter, LlmRequest, ToolCallRequest, ToolSchema};

    #[test]
    fn credential_store_loads_a_private_file_and_redacts_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        std::fs::write(&path, "api_key = sk_test_private_key\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o600);
        }
        std::fs::set_permissions(&path, permissions).unwrap();

        let credential = super::CredentialStore::resolve(None, &path).unwrap();
        assert_eq!(credential.expose(), "sk_test_private_key");
        assert_eq!(format!("{credential}"), "[redacted]");
    }

    #[test]
    fn credential_store_rejects_missing_or_insecure_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert!(super::CredentialStore::resolve(None, &missing).is_err());

        let insecure = dir.path().join("credentials");
        std::fs::write(&insecure, "api_key = sk_test_private_key\n").unwrap();
        let mut permissions = std::fs::metadata(&insecure).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o644);
        }
        std::fs::set_permissions(&insecure, permissions).unwrap();
        assert!(super::CredentialStore::resolve(None, &insecure).is_err());
    }

    #[test]
    fn request_body_maps_harness_vocabulary_to_chat_completions() {
        let request = LlmRequest {
            messages: vec![ChatMessage::User {
                content: "hello".into(),
            }],
            tools: vec![ToolSchema {
                name: "echo".into(),
                description: "Echo text.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}}
                }),
            }],
            model: Some("deepseek-chat".into()),
            metadata: Default::default(),
        };

        let body = super::OpenAiCompatibleAdapter::request_body(&request).unwrap();
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hello");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "echo");
        assert_eq!(body["tools"][0]["function"]["description"], "Echo text.");
    }

    #[test]
    fn request_body_preserves_assistant_tool_calls_for_tool_results() {
        let request = LlmRequest {
            messages: vec![
                ChatMessage::User {
                    content: "use the tool".into(),
                },
                ChatMessage::AssistantToolCall {
                    content: "Calling the tool.".into(),
                    tool_calls: vec![ToolCallRequest {
                        id: "call_1".into(),
                        name: "echo".into(),
                        arguments: json!({"text": "ok"}),
                    }],
                },
                ChatMessage::Tool {
                    tool_call_id: "call_1".into(),
                    content: "ok".into(),
                },
            ],
            tools: Vec::new(),
            model: Some("deepseek-chat".into()),
            metadata: Default::default(),
        };

        let body = super::OpenAiCompatibleAdapter::request_body(&request).unwrap();
        let assistant = &body["messages"][1];
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], "Calling the tool.");
        assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
        assert_eq!(assistant["tool_calls"][0]["type"], "function");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "echo");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            r#"{"text":"ok"}"#
        );
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
        assert_eq!(body["messages"][2]["content"], "ok");
    }

    #[test]
    fn sse_decoder_accumulates_text_tool_calls_and_usage() {
        let mut decoder = super::SseDecoder::new();
        let tool_start = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "echo",
                            "arguments": "{\"text\":"
                        }
                    }]
                }
            }]
        })
        .to_string();
        let tool_end = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "\"ok\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 7,
                "completion_tokens": 3
            }
        })
        .to_string();
        let events = vec![
            decoder
                .ingest_data(r#"{"choices":[{"delta":{"content":"Hello "}}]}"#)
                .unwrap(),
            decoder.ingest_data(&tool_start).unwrap(),
            decoder.ingest_data(&tool_end).unwrap(),
            decoder
                .ingest_data(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
                .unwrap(),
            decoder.ingest_data("[DONE]").unwrap(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        assert!(matches!(
            events.first().unwrap(),
            super::DecoderEvent::Delta(text) if text == "Hello "
        ));
        assert!(matches!(
            events.last().unwrap(),
            super::DecoderEvent::Done { stop_reason, usage }
                if stop_reason.as_deref() == Some("tool_calls")
                    && usage.as_ref().unwrap().input_tokens == Some(7)
                    && usage.as_ref().unwrap().output_tokens == Some(3)
        ));
        let tool = events
            .iter()
            .rev()
            .find_map(|event| match event {
                super::DecoderEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some((id.clone(), name.clone(), arguments.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(tool, ("call_1".into(), "echo".into(), json!({"text":"ok"})));
    }

    #[tokio::test]
    async fn openai_adapter_streams_from_a_local_sse_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (request_tx, request_rx) = mpsc::channel::<String>();

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = socket.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                let headers_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|position| position + 4);
                let Some(headers_end) = headers_end else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]).to_string();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        (name.eq_ignore_ascii_case("content-length"))
                            .then(|| value.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if request.len() >= headers_end + content_length {
                    break;
                }
            }
            request_tx
                .send(String::from_utf8_lossy(&request).to_string())
                .unwrap();

            let first = json!({
                "choices": [{"delta": {"content": "remote hello"}}]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {first}\n\ndata: [DONE]\n\n"
            );
            socket.write_all(response.as_bytes()).unwrap();
            socket.flush().unwrap();
        });

        let adapter = super::OpenAiCompatibleAdapter::new(
            format!("http://127.0.0.1:{port}"),
            super::ApiKey::new("sk_test_remote"),
            "deepseek-chat".into(),
        )
        .unwrap();
        let request = LlmRequest {
            messages: vec![ChatMessage::User {
                content: "hello".into(),
            }],
            tools: Vec::new(),
            model: Some("deepseek-chat".into()),
            metadata: Default::default(),
        };

        let mut stream = adapter.stream(request).await.unwrap();
        let mut frames = Vec::new();
        while let Some(frame) = stream.recv().await {
            frames.push(frame);
        }
        server.join().unwrap();
        let wire_request = request_rx.recv().unwrap();

        assert!(wire_request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk_test_remote"));
        assert!(wire_request.contains("\"model\":\"deepseek-chat\""));
        assert!(wire_request.contains("\"stream\":true"));
        assert!(matches!(
            frames.first(),
            Some(crate::llm::StreamFrame::Delta { text, .. }) if text == "remote hello"
        ));
        assert_eq!(
            frames
                .iter()
                .filter(|frame| matches!(frame, crate::llm::StreamFrame::Done { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn openai_adapter_retries_transient_http_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (requests_tx, requests_rx) = mpsc::channel::<usize>();

        std::thread::spawn(move || {
            let mut accepted = 0;
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let count = socket.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    let Some(headers_end) = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| position + 4)
                    else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..headers_end]).to_string();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            (name.eq_ignore_ascii_case("content-length"))
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if request.len() >= headers_end + content_length {
                        break;
                    }
                }
                accepted += 1;
                requests_tx.send(accepted).unwrap();

                if accepted == 1 {
                    socket
                        .write_all(
                            b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 0\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
                        )
                        .unwrap();
                } else {
                    let event = json!({
                        "choices": [{"delta": {"content": "after retry"}}]
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {event}\n\ndata: [DONE]\n\n"
                    );
                    socket.write_all(response.as_bytes()).unwrap();
                }
                socket.flush().unwrap();
            }
        });

        let adapter = super::OpenAiCompatibleAdapter::new(
            format!("http://127.0.0.1:{port}"),
            super::ApiKey::new("sk_test_retry"),
            "deepseek-chat".into(),
        )
        .unwrap();
        let mut stream = adapter
            .stream(LlmRequest {
                messages: vec![ChatMessage::User {
                    content: "retry".into(),
                }],
                tools: Vec::new(),
                model: Some("deepseek-chat".into()),
                metadata: Default::default(),
            })
            .await
            .unwrap();

        let mut frames = Vec::new();
        while let Some(frame) = stream.recv().await {
            frames.push(frame);
        }
        assert_eq!(requests_rx.recv().unwrap(), 1);
        assert_eq!(requests_rx.recv().unwrap(), 2);
        assert!(matches!(
            frames.first(),
            Some(crate::llm::StreamFrame::Delta { text, .. }) if text == "after retry"
        ));
    }

    #[test]
    fn model_selection_prefers_credentials_and_falls_back_to_local() {
        let missing = std::path::Path::new("missing-credentials-for-test");
        let remote = super::ModelSelection::select(
            Some("sk_test_remote"),
            missing,
            Some("deepseek-reasoner"),
        )
        .unwrap();
        assert!(remote.is_remote);
        assert_eq!(remote.model, "deepseek-reasoner");

        let local = super::ModelSelection::select(None, missing, None).unwrap();
        assert!(!local.is_remote);
        assert_eq!(local.model, "local-null");
    }
}
