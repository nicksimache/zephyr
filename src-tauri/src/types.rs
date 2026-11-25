use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub type Options = BTreeMap<String, Value>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: u16,
    pub num_ctx: u64,
    pub keep_alive: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 47833,
            num_ctx: 4096,
            keep_alive: "5m".into(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), Failure> {
        if self.port == 0 {
            return Err(Failure::invalid("Port must be between 1 and 65535"));
        }
        validate_options(&Options::from([("num_ctx".into(), self.num_ctx.into())]))?;
        validate_keep_alive(&self.keep_alive)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub config: AgentConfig,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub options: Options,
    pub keep_alive: Option<String>,
}

impl AgentConfig {
    pub fn validate(&self) -> Result<(), Failure> {
        if self.name.trim().is_empty() || self.name.chars().count() > 120 {
            return Err(Failure::invalid("Agent name must be 1–120 characters"));
        }
        if self.model.trim().is_empty() || self.model.chars().count() > 240 {
            return Err(Failure::invalid("An installed model name is required"));
        }
        validate_options(&self.options)?;
        if let Some(duration) = &self.keep_alive {
            validate_keep_alive(duration)?;
        }
        Ok(())
    }
}

#[derive(Default, Deserialize)]
pub struct AgentPatch {
    pub name: Option<String>,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub options: Option<Options>,
    #[serde(default, deserialize_with = "nullable")]
    pub keep_alive: Option<Option<String>>,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl AgentPatch {
    pub fn apply(self, config: &mut AgentConfig) {
        if let Some(name) = self.name {
            config.name = name;
        }
        if let Some(model) = self.model {
            config.model = model;
        }
        if let Some(prompt) = self.system_prompt {
            config.system_prompt = prompt;
        }
        if let Some(options) = self.options {
            config.options = options;
        }
        if let Some(duration) = self.keep_alive {
            config.keep_alive = duration;
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(flatten)]
    pub extra: Options,
}

impl Message {
    pub fn text(role: &str, content: String) -> Self {
        Self {
            role: role.into(),
            content,
            extra: Options::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Context {
    pub messages: Vec<Message>,
    pub updated_at: Option<DateTime<Utc>>,
    pub last_sequence: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    Append,
    Replace,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Chat {
    pub model: Option<String>,
    pub context_mode: Option<ContextMode>,
    #[serde(default)]
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub options: Options,
    pub keep_alive: Option<String>,
    pub format: Option<Value>,
    pub tools: Option<Vec<Value>>,
    pub think: Option<Value>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
}

impl Chat {
    pub fn validate(&self) -> Result<(), Failure> {
        if self.messages.is_empty()
            || self
                .messages
                .iter()
                .any(|m| !matches!(m.role.as_str(), "system" | "user" | "assistant" | "tool"))
        {
            return Err(Failure::invalid(
                "Supply messages with system, user, assistant, or tool roles",
            ));
        }
        if self
            .format
            .as_ref()
            .is_some_and(|v| !v.is_object() && v.as_str() != Some("json"))
        {
            return Err(Failure::invalid(
                "Format must be \"json\" or a JSON schema object",
            ));
        }
        validate_options(&self.options)?;
        if let Some(duration) = &self.keep_alive {
            validate_keep_alive(duration)?;
        }
        Ok(())
    }
}

fn validate_options(options: &Options) -> Result<(), Failure> {
    if options
        .get("num_ctx")
        .is_some_and(|v| !matches!(v.as_u64(), Some(128..=2_000_000)))
    {
        return Err(Failure::invalid(
            "num_ctx must be an integer between 128 and 2000000",
        ));
    }
    if options.contains_key("truncate") || options.contains_key("shift") {
        return Err(Failure::invalid(
            "Zephyr controls context truncation and shifting",
        ));
    }
    Ok(())
}

pub fn validate_keep_alive(value: &str) -> Result<(), Failure> {
    let value = value.trim();
    let first_digit = value.chars().next().is_some_and(|c| c.is_ascii_digit());
    if value == "-1"
        || value == "0"
        || (first_digit
            && value.len() <= 24
            && value
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'n' | 'u' | 'm' | 's' | 'h'))
            && value.ends_with(['h', 'm', 's']))
    {
        return Ok(());
    }
    Err(Failure::invalid(
        "Use a duration such as 30s, 5m, or 1h, or use 0 or -1",
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_status: Option<u16>,
}

impl Failure {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            upstream_status: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }
    pub fn missing(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }
    pub fn storage(message: impl Into<String>) -> Self {
        Self::new("storage_error", message)
    }
    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::new("cancelled", message)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Queued,
    Generating,
    Completed,
    Error,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerExecution {
    pub id: Uuid,
    pub name: String,
    pub request_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestMeta {
    pub request_id: Uuid,
    pub sequence: u64,
    pub agent_id: Option<Uuid>,
    pub agent_name: Option<String>,
    pub context_mode: Option<ContextMode>,
    pub peer: Option<PeerExecution>,
    pub model: String,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: Status,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Metrics {
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub context_limit: Option<u64>,
    pub context_utilization: Option<f64>,
    pub load_ms: Option<f64>,
    pub prompt_ms: Option<f64>,
    pub generation_ms: Option<f64>,
    pub tokens_per_second: Option<f64>,
    pub provider_total_ms: Option<f64>,
    pub request_ms: Option<f64>,
    pub queue_ms: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub schema_version: u8,
    #[serde(flatten)]
    pub meta: RequestMeta,
    pub model_version: Option<Value>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    pub messages_sent: Vec<Message>,
    pub client_request: Value,
    pub raw_request: Option<Value>,
    pub raw_response: Option<Value>,
    pub response: Option<Message>,
    pub reasoning: Option<String>,
    pub inference_options: Options,
    pub keep_alive: Option<String>,
    pub metrics: Metrics,
    pub error: Option<Failure>,
    pub done_reason: Option<String>,
    pub context_committed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestSummary {
    #[serde(flatten)]
    pub meta: RequestMeta,
    pub model_version: Option<Value>,
    pub metrics: Metrics,
    pub error: Option<Failure>,
    pub done_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LogQuery {
    pub before: Option<u64>,
    pub limit: Option<usize>,
    pub agent: Option<Uuid>,
    pub peer: Option<Uuid>,
    pub model: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub status: Option<Status>,
    #[serde(default)]
    pub full: bool,
}

impl LogQuery {
    pub fn matches(&self, meta: &RequestMeta) -> bool {
        self.agent.is_none_or(|agent| meta.agent_id == Some(agent))
            && self
                .peer
                .is_none_or(|id| meta.peer.as_ref().is_some_and(|peer| peer.id == id))
            && self.model.as_ref().is_none_or(|model| &meta.model == model)
            && self
                .run_id
                .as_ref()
                .is_none_or(|run| meta.run_id.as_ref() == Some(run))
            && self
                .task_id
                .as_ref()
                .is_none_or(|task| meta.task_id.as_ref() == Some(task))
            && self.status.is_none_or(|status| meta.status == status)
    }
}

#[derive(Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_before: Option<u64>,
}

#[derive(Default, Serialize)]
pub struct TokenTotals {
    pub requests: u64,
    pub requests_without_counts: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl TokenTotals {
    pub fn add(&mut self, metrics: &Metrics) {
        self.requests += 1;
        if metrics.input_tokens.is_none() || metrics.output_tokens.is_none() {
            self.requests_without_counts += 1;
        }
        self.input_tokens = self
            .input_tokens
            .saturating_add(metrics.input_tokens.unwrap_or_default());
        if let Some(cached) = metrics.cached_input_tokens {
            self.cached_input_tokens = Some(
                self.cached_input_tokens
                    .unwrap_or_default()
                    .saturating_add(cached),
            );
        }
        self.output_tokens = self
            .output_tokens
            .saturating_add(metrics.output_tokens.unwrap_or_default());
        self.total_tokens = self.input_tokens.saturating_add(self.output_tokens);
    }
}

#[derive(Serialize)]
pub struct TokenUsage {
    pub totals: TokenTotals,
    pub models: BTreeMap<String, TokenTotals>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub size: u64,
    pub size_vram: Option<u64>,
    pub context_length: Option<u64>,
    pub modified_at: Option<String>,
    pub expires_at: Option<String>,
    pub details: Option<Value>,
    pub remote_model: Option<String>,
    pub remote_host: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Activity {
    #[serde(flatten)]
    pub meta: RequestMeta,
    pub messages: Vec<Message>,
    pub output: Message,
    pub frame: u64,
}
