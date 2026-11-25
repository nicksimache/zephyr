use std::{io, time::Instant};

use axum::{
    body::{Body, Bytes},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use chrono::Utc;
use futures_util::TryStreamExt;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;
use uuid::Uuid;

use crate::{
    api::{LOG_ID, REQUEST_ID},
    gateway::Gateway,
    types::{Chat, Failure, Metrics, PeerExecution, Record, RequestMeta, Status},
};

pub struct ForwardAudit {
    gateway: Gateway,
    record: Option<Record>,
    frames: Vec<Value>,
    started: Instant,
}

impl ForwardAudit {
    pub async fn start(
        gateway: Gateway,
        peer: PeerExecution,
        raw: &Value,
    ) -> Result<Self, Failure> {
        let call: Chat = serde_json::from_value(raw.clone())
            .map_err(|error| Failure::invalid(error.to_string()))?;
        call.validate()?;
        if call.context_mode.is_some() {
            return Err(Failure::invalid(
                "context_mode is only available for agent requests",
            ));
        }
        let model = call
            .model
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| Failure::invalid("A model is required for stateless requests"))?;
        let now = Utc::now();
        let record = Record {
            schema_version: 1,
            meta: RequestMeta {
                request_id: Uuid::new_v4(),
                sequence: 0,
                agent_id: None,
                agent_name: None,
                context_mode: None,
                peer: Some(peer),
                model,
                run_id: call.run_id,
                task_id: call.task_id,
                requested_at: now,
                started_at: Some(now),
                completed_at: None,
                status: Status::Generating,
            },
            model_version: None,
            system_prompt: None,
            messages_sent: call.messages,
            client_request: raw.clone(),
            raw_request: Some(raw.clone()),
            raw_response: None,
            response: None,
            reasoning: None,
            inference_options: call.options,
            keep_alive: call.keep_alive,
            metrics: Metrics::default(),
            error: None,
            done_reason: None,
            context_committed: false,
        };
        let record = gateway.store().run(move |disk| disk.admit(record)).await?;
        Ok(Self {
            gateway,
            record: Some(record),
            frames: Vec::new(),
            started: Instant::now(),
        })
    }

    pub async fn reply(
        mut self,
        upstream: Result<reqwest::Response, Failure>,
    ) -> Result<Response, Failure> {
        let upstream = match upstream {
            Ok(upstream) => upstream,
            Err(error) => {
                let id = self.record.as_ref().unwrap().meta.request_id;
                self.finish(Status::Error, Some(error.clone())).await?;
                let mut response = axum::response::IntoResponse::into_response(error);
                insert_log_id(response.headers_mut(), id);
                return Ok(response);
            }
        };
        let status = upstream.status();
        let headers = upstream.headers().clone();
        let record = self.record.as_mut().unwrap();
        let id = record.meta.request_id;
        record.meta.peer.as_mut().unwrap().request_id = headers
            .get(REQUEST_ID)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Uuid::parse_str(value).ok());

        let streaming = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/x-ndjson"));
        let body = if streaming {
            Body::from_stream(self.stream(upstream))
        } else {
            let bytes = match upstream.bytes().await {
                Ok(bytes) => bytes,
                Err(error) => {
                    let error = Failure::new(
                        "peer_unreachable",
                        format!("The paired computer stopped responding: {error}"),
                    );
                    self.finish(Status::Error, Some(error.clone())).await?;
                    let mut response = axum::response::IntoResponse::into_response(error);
                    insert_log_id(response.headers_mut(), id);
                    return Ok(response);
                }
            };
            let raw = serde_json::from_slice::<Value>(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
            let (outcome, error) = self.read_result(&raw, status);
            self.record.as_mut().unwrap().raw_response = Some(raw);
            self.finish(outcome, error).await?;
            Body::from(bytes)
        };
        let mut response = peer_response(status, &headers, body);
        insert_log_id(response.headers_mut(), id);
        Ok(response)
    }

    fn stream(
        mut self,
        upstream: reqwest::Response,
    ) -> impl futures_util::Stream<Item = Result<Bytes, io::Error>> {
        let status = upstream.status();
        async_stream::try_stream! {
            let input = upstream.bytes_stream().map_err(io::Error::other);
            let mut reader = BufReader::new(StreamReader::new(input));
            let mut line = Vec::new();
            loop {
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) => {
                        self.finish(Status::Error, Some(Failure::new("peer_unreachable", error.to_string())))
                            .await.map_err(|error| io::Error::other(error.message))?;
                        Err(error)?;
                        break;
                    }
                }
                if let Ok(frame) = serde_json::from_slice::<Value>(&line) {
                    let terminal = frame["type"] == "done";
                    if terminal {
                        let (outcome, error) = self.read_result(&frame, status);
                        self.frames.push(frame);
                        self.finish(outcome, error).await.map_err(|error| io::Error::other(error.message))?;
                        yield Bytes::from(line);
                        return;
                    }
                    self.frames.push(frame);
                }
                yield Bytes::from(std::mem::take(&mut line));
            }
            self.finish(Status::Error, Some(Failure::new("upstream_error", "The paired stream ended before a result arrived")))
                .await.map_err(|error| io::Error::other(error.message))?;
        }
    }

    fn read_result(&mut self, result: &Value, status: StatusCode) -> (Status, Option<Failure>) {
        let record = self.record.as_mut().unwrap();
        if let Some(name) = result["model"].as_str() {
            record.meta.model = name.into();
        }
        record.model_version = result
            .get("model_version")
            .filter(|value| !value.is_null())
            .cloned();
        if let Ok(metrics) = serde_json::from_value(result["metrics"].clone()) {
            record.metrics = metrics;
        }
        record.response = serde_json::from_value(result["message"].clone()).ok();
        record.reasoning = result["message"]["thinking"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        record.done_reason = result["done_reason"].as_str().map(str::to_owned);
        let outcome = match result["status"].as_str() {
            Some("completed") if status.is_success() => return (Status::Completed, None),
            Some("cancelled") => Status::Cancelled,
            Some("interrupted") => Status::Interrupted,
            _ => Status::Error,
        };
        let error =
            serde_json::from_value::<Failure>(result["error"].clone()).unwrap_or_else(|_| {
                let mut error = Failure::new(
                    "upstream_error",
                    "The paired computer did not return a completed result",
                );
                error.upstream_status = Some(status.as_u16());
                error
            });
        (outcome, Some(error))
    }

    async fn finish(&mut self, status: Status, error: Option<Failure>) -> Result<(), Failure> {
        if let Some(record) = self.take_record(status, error) {
            self.gateway.finish_forwarded(record).await?;
        }
        Ok(())
    }

    fn take_record(&mut self, status: Status, error: Option<Failure>) -> Option<Record> {
        let mut record = self.record.take()?;
        if !self.frames.is_empty() {
            record.raw_response = Some(json!({"frames": std::mem::take(&mut self.frames)}));
        }
        record.meta.status = status;
        record.meta.completed_at = Some(Utc::now());
        record.error = error;
        record.metrics.request_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
        Some(record)
    }
}

impl Drop for ForwardAudit {
    fn drop(&mut self) {
        if let Some(record) = self.take_record(
            Status::Cancelled,
            Some(Failure::cancelled("HTTP client disconnected")),
        ) {
            let gateway = self.gateway.clone();
            // The journal marks this request interrupted if the process exits before the write.
            tokio::spawn(async move {
                if let Err(error) = gateway.finish_forwarded(record).await {
                    eprintln!("Zephyr forwarding log: {}", error.message);
                }
            });
        }
    }
}

fn insert_log_id(headers: &mut HeaderMap, id: Uuid) {
    headers.insert(
        HeaderName::from_static(LOG_ID),
        HeaderValue::from_str(&id.to_string()).unwrap(),
    );
}

pub fn peer_response(status: StatusCode, headers: &HeaderMap, body: Body) -> Response {
    let mut response = Response::builder().status(status);
    for name in [header::CONTENT_TYPE, HeaderName::from_static(REQUEST_ID)] {
        if let Some(value) = headers.get(&name) {
            response = response.header(name, value);
        }
    }
    response
        .header(header::CACHE_CONTROL, "no-store")
        .body(body)
        .unwrap()
}
