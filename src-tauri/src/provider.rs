use std::{io, path::PathBuf, pin::Pin, process::Stdio, time::Duration};

use async_trait::async_trait;
use futures_util::{Stream, TryStreamExt};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;

use crate::types::{Failure, Model};

pub const TEST_MODEL: &str = "qwen2.5:0.5b";
pub const MIN_VERSION: &str = "0.40.18";
const HOST: &str = "http://127.0.0.1:11434";

pub type Frames = Pin<Box<dyn Stream<Item = Result<Value, ProviderError>> + Send>>;

#[derive(Debug)]
pub struct ProviderError {
    pub failure: Failure,
    pub raw: Option<Value>,
}

impl From<Failure> for ProviderError {
    fn from(failure: Failure) -> Self {
        Self { failure, raw: None }
    }
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn version(&self) -> Result<String, Failure>;
    async fn models(&self, loaded: bool) -> Result<Vec<Model>, Failure>;
    async fn chat(&self, request: &Value) -> Result<Frames, ProviderError>;
    async fn residency(
        &self,
        model: &str,
        load: bool,
        keep_alive: &str,
        num_ctx: u64,
    ) -> Result<Value, Failure>;
}

#[derive(Clone)]
pub struct Ollama {
    client: Client,
}

impl Ollama {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .read_timeout(Duration::from_secs(300))
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .build()?,
        })
    }

    async fn get(&self, path: &str) -> Result<Value, Failure> {
        let response = self
            .client
            .get(format!("{HOST}{path}"))
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .map_err(transport)?;
        decode(response).await.map_err(|error| error.failure)
    }

    pub async fn pull_test_model(&self) -> Result<Value, Failure> {
        let response = self
            .client
            .post(format!("{HOST}/api/pull"))
            .json(&json!({"model": TEST_MODEL, "stream": false}))
            .timeout(Duration::from_secs(1800))
            .send()
            .await
            .map_err(transport)?;
        decode(response).await.map_err(|error| error.failure)
    }

    pub async fn start(&self) -> Result<String, Failure> {
        if let Ok(version) = self.version().await {
            return Ok(version);
        }
        let executable = installed_path().ok_or_else(|| {
            Failure::new(
                "ollama_missing",
                "Install Ollama to start the local inference engine",
            )
        })?;
        let mut command = quiet_command(executable);
        command
            .arg("serve")
            .env("OLLAMA_HOST", "127.0.0.1:11434")
            .env("OLLAMA_NO_CLOUD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn()
            .map_err(|error| Failure::new("ollama_start_failed", error.to_string()))?;
        for _ in 0..35 {
            if let Ok(version) = self.version().await {
                return Ok(version);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(Failure::new(
            "ollama_unavailable",
            "Ollama did not respond on 127.0.0.1:11434",
        ))
    }
}

#[async_trait]
impl Provider for Ollama {
    async fn version(&self) -> Result<String, Failure> {
        self.get("/api/version")
            .await?
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Failure::new("upstream_error", "Ollama did not report a version"))
    }

    async fn models(&self, loaded: bool) -> Result<Vec<Model>, Failure> {
        let json = self
            .get(if loaded { "/api/ps" } else { "/api/tags" })
            .await?;
        let models: Vec<Model> = serde_json::from_value(json["models"].clone())
            .map_err(|error| Failure::new("upstream_error", error.to_string()))?;
        Ok(models
            .into_iter()
            .filter(|model| {
                model.remote_host.as_deref().unwrap_or("").is_empty()
                    && model.remote_model.as_deref().unwrap_or("").is_empty()
                    && !model.name.ends_with("-cloud")
                    && !model.name.ends_with(":cloud")
            })
            .collect())
    }

    async fn chat(&self, request: &Value) -> Result<Frames, ProviderError> {
        let response = self
            .client
            .post(format!("{HOST}/api/chat"))
            .json(request)
            .send()
            .await
            .map_err(transport)?;
        if !response.status().is_success() {
            return Err(rejected(response).await);
        }
        let input = response.bytes_stream().map_err(io::Error::other);
        let mut reader = BufReader::new(StreamReader::new(input));
        Ok(Box::pin(async_stream::try_stream! {
            let mut line = Vec::new();
            loop {
                line.clear();
                let size = reader.read_until(b'\n', &mut line).await
                    .map_err(|error| Failure::new("ollama_unavailable", error.to_string()))?;
                if size == 0 { break; }
                if line.iter().all(u8::is_ascii_whitespace) { continue; }
                let frame = serde_json::from_slice(&line).map_err(|error| ProviderError {
                    failure: Failure::new("upstream_error", format!("Invalid Ollama stream: {error}")),
                    raw: Some(Value::String(String::from_utf8_lossy(&line).into_owned())),
                })?;
                yield frame;
            }
        }))
    }

    async fn residency(
        &self,
        model: &str,
        load: bool,
        keep_alive: &str,
        num_ctx: u64,
    ) -> Result<Value, Failure> {
        let keep_alive = if load {
            // A zero request default should still allow the explicit Load action.
            if keep_alive == "0" {
                json!("5m")
            } else {
                duration_value(keep_alive)
            }
        } else {
            json!(0)
        };
        let response = self.client.post(format!("{HOST}/api/generate"))
            .json(&json!({"model":model, "stream":false, "keep_alive":keep_alive, "options":{"num_ctx":num_ctx}}))
            .send().await.map_err(transport)?;
        decode(response).await.map_err(|error| error.failure)
    }
}

pub fn duration_value(duration: &str) -> Value {
    match duration.trim() {
        "-1" => json!(-1),
        "0" => json!(0),
        value => json!(value),
    }
}

pub fn supported(version: &str) -> bool {
    fn triplet(value: &str) -> Option<(u64, u64, u64)> {
        let mut parts = value.trim_start_matches('v').split(['.', '-', '+']);
        Some((
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ))
    }
    triplet(version).is_some_and(|value| value >= triplet(MIN_VERSION).unwrap())
}

pub async fn find_model(provider: &dyn Provider, name: &str) -> Result<Model, Failure> {
    let wanted = if name.rsplit('/').next().unwrap_or(name).contains(':') {
        name.to_owned()
    } else {
        format!("{name}:latest")
    };
    provider
        .models(false)
        .await?
        .into_iter()
        .find(|model| model.name.eq_ignore_ascii_case(&wanted))
        .ok_or_else(|| {
            Failure::new(
                "model_unavailable",
                format!("Local model {name} is not installed"),
            )
        })
}

pub fn classify(message: &str, status: Option<u16>) -> Failure {
    let lower = message.to_ascii_lowercase();
    let code = if status == Some(413)
        || lower.contains("context length")
        || lower.contains("context window")
        || lower.contains("input length exceeds")
        || lower.contains("context size")
    {
        "context_limit"
    } else if lower.contains("memory")
        || lower.contains("allocation failed")
        || lower.contains("cuda out of")
    {
        "insufficient_memory"
    } else if status == Some(404) {
        "model_unavailable"
    } else {
        "upstream_error"
    };
    let mut error = Failure::new(code, message);
    error.upstream_status = status;
    error
}

fn transport(error: reqwest::Error) -> Failure {
    Failure::new(
        "ollama_unavailable",
        format!("Ollama at 127.0.0.1:11434: {error}"),
    )
}

async fn decode(response: Response) -> Result<Value, ProviderError> {
    if response.status().is_success() {
        return response
            .json()
            .await
            .map_err(|error| Failure::new("upstream_error", error.to_string()).into());
    }
    Err(rejected(response).await)
}

async fn rejected(response: Response) -> ProviderError {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let raw = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text));
    let message = raw
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| raw.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Ollama rejected the request");
    ProviderError {
        failure: classify(message, Some(status)),
        raw: Some(raw),
    }
}

pub fn installed_path() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ollama.exe"
    } else {
        "ollama"
    };
    let local_install = std::env::var_os("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("Programs/Ollama").join(name));
    let path_entries = std::env::var_os("PATH");
    let mut paths = local_install.into_iter().chain(
        path_entries
            .as_ref()
            .into_iter()
            .flat_map(std::env::split_paths)
            .map(|directory| directory.join(name)),
    );
    paths.find(|path| path.is_file())
}

pub fn quiet_command(path: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(path);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}
