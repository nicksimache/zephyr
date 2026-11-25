use std::{
    collections::HashMap,
    sync::{Arc, Mutex as SyncMutex},
    time::Instant,
};

use chrono::Utc;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    provider::{self, duration_value, find_model, Ollama, Provider, MIN_VERSION},
    store::{Disk, Store},
    types::*,
};

const INFERENCE_CAPACITY: usize = 128;

#[derive(Clone)]
pub struct Gateway(Arc<Shared>);

struct Shared {
    pub store: Store,
    provider: Arc<dyn Provider>,
    ollama: Ollama,
    pub port: u16,
    admission: Mutex<HashMap<Uuid, Running>>,
    engine: Mutex<()>,
    setup: Mutex<()>,
    inbox: mpsc::UnboundedSender<Job>,
    events: broadcast::Sender<Value>,
    closing: CancellationToken,
    stopped: watch::Receiver<bool>,
}

#[derive(Clone)]
struct Running {
    progress: Arc<SyncMutex<Activity>>,
    cancel: Cancellation,
}

#[derive(Clone, Default)]
struct Cancellation {
    token: CancellationToken,
    reason: Arc<SyncMutex<Option<String>>>,
}

impl Cancellation {
    fn cancel(&self, reason: &str) {
        self.reason
            .lock()
            .unwrap()
            .get_or_insert_with(|| reason.to_owned());
        self.token.cancel();
    }

    fn failure(&self) -> Failure {
        let reason = self
            .reason
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "Request cancelled".into());
        Failure::cancelled(reason)
    }
}

pub struct ClientGuard(Cancellation);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.cancel("HTTP client disconnected");
    }
}

pub struct Submission {
    pub id: Uuid,
    pub start: Value,
    pub responses: mpsc::UnboundedReceiver<Value>,
    pub guard: ClientGuard,
}

struct Job {
    call: Chat,
    agent: Option<Agent>,
    record: Record,
    running: Running,
    reply: mpsc::UnboundedSender<Value>,
    admitted: Instant,
}

#[derive(Default)]
struct Output {
    frames: Vec<Value>,
    upstream_error: Option<Value>,
}

impl Gateway {
    pub fn launch(store: Store, port: u16, provider: Arc<dyn Provider>, ollama: Ollama) -> Self {
        let (inbox, reader) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(256);
        let (finished, stopped) = watch::channel(false);
        let gateway = Self(Arc::new(Shared {
            store,
            port,
            provider,
            ollama,
            admission: Mutex::new(HashMap::new()),
            engine: Mutex::new(()),
            setup: Mutex::new(()),
            inbox,
            events,
            closing: CancellationToken::new(),
            stopped,
        }));
        let worker = gateway.clone();
        tokio::spawn(async move {
            worker.process(reader).await;
            finished.send_replace(true);
        });
        gateway
    }

    pub fn store(&self) -> &Store {
        &self.0.store
    }
    pub fn port(&self) -> u16 {
        self.0.port
    }
    pub fn events(&self) -> broadcast::Receiver<Value> {
        self.0.events.subscribe()
    }
    pub fn closing(&self) -> CancellationToken {
        self.0.closing.clone()
    }

    pub async fn stop(&self) {
        self.0.closing.cancel();
        let mut stopped = self.0.stopped.clone();
        let _ = stopped.wait_for(|stopped| *stopped).await;
    }

    pub async fn installed_models(&self, loaded: bool) -> Result<Vec<Model>, Failure> {
        self.0.provider.models(loaded).await
    }

    pub async fn version(&self) -> Result<String, Failure> {
        self.0.provider.version().await
    }

    pub async fn start_provider(&self) -> Result<String, Failure> {
        let _setup = self.0.setup.lock().await;
        let result = self.0.ollama.start().await;
        self.emit(json!({"type":"models_changed"}));
        result
    }

    pub async fn pull_model(&self) -> Result<Value, Failure> {
        let _setup = self.0.setup.lock().await;
        let result = self.0.ollama.pull_test_model().await;
        self.emit(json!({"type":"models_changed"}));
        result
    }

    pub async fn residency(&self, model: String, load: bool) -> Result<Value, Failure> {
        let _engine = tokio::select! {
            _ = self.0.closing.cancelled() => return Err(unavailable()),
            engine = self.0.engine.lock() => engine,
        };
        let model = find_model(&*self.0.provider, &model).await?;
        let settings = self.store().run(Disk::settings).await?;
        let result = tokio::select! {
            _ = self.0.closing.cancelled() => Err(unavailable()),
            value = self.0.provider.residency(&model.name, load, &settings.keep_alive, settings.num_ctx) => value,
        };
        self.emit(json!({"type":"models_changed"}));
        result
    }

    pub async fn create_agent(&self, mut config: AgentConfig) -> Result<Agent, Failure> {
        config.validate()?;
        config.model = find_model(&*self.0.provider, &config.model).await?.name;
        let _admission = self.0.admission.lock().await;
        let agent = self.store().run(move |disk| disk.create(config)).await?;
        self.emit(json!({"type":"agents_changed","agent_id":agent.id}));
        Ok(agent)
    }

    pub async fn update_agent(&self, id: Uuid, patch: AgentPatch) -> Result<Agent, Failure> {
        let running = self.0.admission.lock().await;
        ensure_idle(&running, id)?;
        let mut agent = self.store().run(move |disk| disk.agent(id, false)).await?;
        let model = agent.config.model.clone();
        patch.apply(&mut agent.config);
        agent.config.validate()?;
        if agent.config.model != model {
            agent.config.model = find_model(&*self.0.provider, &agent.config.model)
                .await?
                .name;
        }
        let agent = self.store().run(move |disk| disk.set_agent(agent)).await?;
        self.emit(json!({"type":"agents_changed","agent_id":id}));
        Ok(agent)
    }

    pub async fn delete_agent(&self, id: Uuid) -> Result<(), Failure> {
        let running = self.0.admission.lock().await;
        ensure_idle(&running, id)?;
        self.store()
            .run(move |disk| {
                let mut agent = disk.agent(id, false)?;
                agent.deleted = true;
                disk.set_agent(agent)
            })
            .await?;
        self.emit(json!({"type":"agents_changed","agent_id":id}));
        Ok(())
    }

    pub async fn reset(&self, id: Uuid) -> Result<Context, Failure> {
        let running = self.0.admission.lock().await;
        ensure_idle(&running, id)?;
        let context = self.store().run(move |disk| disk.reset(id)).await?;
        self.emit(json!({"type":"context_reset","agent_id":id}));
        Ok(context)
    }

    pub async fn activities(&self) -> Vec<Activity> {
        let running = self.0.admission.lock().await;
        let mut list: Vec<_> = running
            .values()
            .map(|live| live.progress.lock().unwrap().clone())
            .collect();
        list.sort_unstable_by_key(|activity| activity.meta.sequence);
        list
    }

    pub async fn counts(&self) -> (usize, usize) {
        let running = self.0.admission.lock().await;
        let active = running
            .values()
            .filter(|live| live.progress.lock().unwrap().meta.status == Status::Generating)
            .count();
        (active, running.len() - active)
    }

    pub async fn cancel(&self, id: Uuid) -> Result<(), Failure> {
        let running = self.0.admission.lock().await;
        let request = running
            .get(&id)
            .ok_or_else(|| Failure::missing("No active request has that ID"))?;
        request.cancel.cancel("Cancelled by caller");
        Ok(())
    }

    pub async fn admit(
        &self,
        agent_id: Option<Uuid>,
        call: Chat,
        raw: Value,
    ) -> Result<Submission, Failure> {
        call.validate()?;
        let mut running = self.0.admission.lock().await;
        if self.0.closing.is_cancelled() {
            return Err(unavailable());
        }
        if running.len() >= INFERENCE_CAPACITY {
            return Err(Failure::new(
                "queue_full",
                "The local inference queue has reached its capacity",
            ));
        }

        let agent = match agent_id {
            Some(id) => {
                if call.model.is_some() {
                    return Err(Failure::invalid(
                        "Stateful requests use the agent's configured model",
                    ));
                }
                Some(self.store().run(move |disk| disk.agent(id, false)).await?)
            }
            None => None,
        };
        if agent.is_none() && call.context_mode.is_some() {
            return Err(Failure::invalid(
                "context_mode is only available for agent requests",
            ));
        }
        let model = agent
            .as_ref()
            .map(|agent| agent.config.model.clone())
            .or_else(|| call.model.clone())
            .filter(|model| !model.trim().is_empty())
            .ok_or_else(|| Failure::invalid("A model is required for stateless requests"))?;
        let settings = self.store().run(Disk::settings).await?;
        let mut options = Options::from([("num_ctx".into(), json!(settings.num_ctx))]);
        if let Some(agent) = &agent {
            options.extend(agent.config.options.clone());
        }
        options.extend(call.options.clone());
        let keep_alive = call
            .keep_alive
            .as_ref()
            .or_else(|| agent.as_ref().and_then(|a| a.config.keep_alive.as_ref()))
            .cloned()
            .unwrap_or(settings.keep_alive);
        let id = Uuid::new_v4();
        let record = Record {
            schema_version: 1,
            meta: RequestMeta {
                request_id: id,
                sequence: 0,
                agent_id,
                agent_name: agent.as_ref().map(|agent| agent.config.name.clone()),
                context_mode: agent
                    .as_ref()
                    .map(|_| call.context_mode.unwrap_or(ContextMode::Append)),
                peer: None,
                model,
                run_id: call.run_id.clone(),
                task_id: call.task_id.clone(),
                requested_at: Utc::now(),
                started_at: None,
                completed_at: None,
                status: Status::Queued,
            },
            model_version: None,
            system_prompt: agent
                .as_ref()
                .map(|agent| agent.config.system_prompt.clone())
                .filter(|prompt| !prompt.is_empty()),
            messages_sent: Vec::new(),
            client_request: raw,
            raw_request: None,
            raw_response: None,
            response: None,
            reasoning: None,
            inference_options: options,
            keep_alive: Some(keep_alive),
            metrics: Metrics::default(),
            error: None,
            done_reason: None,
            context_committed: false,
        };

        let admitted = Instant::now();
        let record = self.store().run(move |disk| disk.admit(record)).await?;
        let activity = Activity {
            meta: record.meta.clone(),
            messages: call.messages.clone(),
            output: Message::text("assistant", String::new()),
            frame: 0,
        };
        let live = Running {
            progress: Arc::new(SyncMutex::new(activity.clone())),
            cancel: Cancellation::default(),
        };
        let (reply, responses) = mpsc::unbounded_channel();
        let guard = ClientGuard(live.cancel.clone());
        running.insert(id, live.clone());
        self.emit(json!({"type":"queued","activity": activity}));
        let start = json!({"type":"start","request_id":id,"sequence":record.meta.sequence,"status":"queued"});
        self.0
            .inbox
            .send(Job {
                call,
                agent,
                record,
                running: live,
                reply,
                admitted,
            })
            .map_err(|_| unavailable())?;
        Ok(Submission {
            id,
            start,
            responses,
            guard,
        })
    }

    fn emit(&self, event: Value) {
        let _ = self.0.events.send(event);
    }

    pub async fn finish_forwarded(&self, record: Record) -> Result<(), Failure> {
        let event = json!({"type":"finished","meta":record.meta,"status":record.meta.status,"error":record.error});
        self.store()
            .run(move |disk| disk.finish(record, None))
            .await?;
        self.emit(event);
        Ok(())
    }

    async fn process(&self, mut queue: mpsc::UnboundedReceiver<Job>) {
        loop {
            let job = tokio::select! {
                biased;
                _ = self.0.closing.cancelled() => break,
                job = queue.recv() => job,
            };
            let Some(mut job) = job else {
                return;
            };
            let cancel = job.running.cancel.clone();
            let mut output = Output::default();
            let mut next_context = None;
            let terminal = tokio::select! {
                biased;
                _ = self.0.closing.cancelled() => Err(Failure::cancelled("Zephyr is shutting down")),
                _ = cancel.token.cancelled() => Err(cancel.failure()),
                result = self.infer(&mut job, &mut output, &mut next_context) => result,
            };
            self.finish(job, terminal, output, next_context).await;
        }
        // Admission holds the lock until its journal and enqueue have both completed.
        {
            let _admission = self.0.admission.lock().await;
            queue.close();
        }
        while let Some(job) = queue.recv().await {
            self.finish(
                job,
                Err(Failure::cancelled("Zephyr is shutting down")),
                Output::default(),
                None,
            )
            .await;
        }
    }

    async fn infer(
        &self,
        job: &mut Job,
        output: &mut Output,
        next_context: &mut Option<Context>,
    ) -> Result<(), Failure> {
        let _engine = self.0.engine.lock().await;
        let id = job.record.meta.request_id;
        let start = Utc::now();
        job.record.meta.started_at = Some(start);
        job.record.meta.status = Status::Generating;
        job.record.metrics.queue_ms = Some(milliseconds(job.admitted.elapsed()));
        {
            let mut activity = job.running.progress.lock().unwrap();
            activity.meta = job.record.meta.clone();
            self.emit(json!({"type":"generating","activity":*activity}));
        }

        let version = self.0.provider.version().await?;
        if !provider::supported(&version) {
            return Err(Failure::new("ollama_upgrade_required",
                format!("Ollama {MIN_VERSION} or newer is required to disable upstream context truncation (found {version})")));
        }
        let model = find_model(&*self.0.provider, &job.record.meta.model).await?;
        job.record.meta.model = model.name.clone();
        job.record.model_version = Some(json!({
            "ollama_version":version,"digest":model.digest,"details":model.details,"modified_at":model.modified_at
        }));

        let mut context = match (&job.agent, job.record.meta.context_mode) {
            (Some(agent), Some(ContextMode::Append)) => {
                let id = agent.id;
                self.store().run(move |disk| disk.context(id)).await?
            }
            _ => Context::default(),
        };
        context.messages.extend(job.call.messages.iter().cloned());
        let system = job.record.system_prompt.as_deref();
        let messages: Vec<_> = system
            .into_iter()
            .map(|prompt| Message::text("system", prompt.into()))
            .chain(context.messages.iter().cloned())
            .collect();
        let mut raw = json!({
            "model":model.name,"messages":messages,"options":job.record.inference_options,
            "keep_alive":duration_value(job.record.keep_alive.as_deref().unwrap()),"stream":true,
            "truncate":false,"shift":false
        });
        if let Some(format) = &job.call.format {
            raw["format"] = format.clone();
        }
        if let Some(tools) = &job.call.tools {
            raw["tools"] = json!(tools);
        }
        if let Some(think) = &job.call.think {
            raw["think"] = think.clone();
        }
        job.record.metrics.context_limit = job
            .record
            .inference_options
            .get("num_ctx")
            .and_then(Value::as_u64);
        job.record.raw_request = Some(raw);
        job.record.messages_sent = messages;
        let record = job.record.clone();
        self.store().run(move |disk| disk.journal(&record)).await?;

        let opened = self
            .0
            .provider
            .chat(job.record.raw_request.as_ref().unwrap())
            .await;
        let mut frames = opened.map_err(|error| {
            output.upstream_error = error.raw;
            error.failure
        })?;
        let mut assistant = Message::text("assistant", String::new());
        while let Some(frame) = frames.next().await {
            let frame = frame.map_err(|error| {
                output.upstream_error = error.raw;
                error.failure
            })?;
            output.frames.push(frame);
            let frame = output.frames.last().unwrap();
            if let Some(error) = frame["error"].as_str() {
                return Err(provider::classify(error, None));
            }
            if let Some(message) = frame.get("message") {
                merge_message(&mut assistant, message);
                let ordinal = {
                    let mut activity = job.running.progress.lock().unwrap();
                    merge_message(&mut activity.output, message);
                    activity.frame += 1;
                    activity.frame
                };
                let event =
                    json!({"type":"delta","request_id":id,"frame":ordinal,"message":message});
                self.emit(event.clone());
                if job.call.stream {
                    let _ = job.reply.send(event);
                }
            }
            if frame["done"] == true {
                apply_metrics(&mut job.record, frame)?;
                if let Ok(Ok(loaded)) = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    self.0.provider.models(true),
                )
                .await
                {
                    if let Some(limit) = loaded
                        .into_iter()
                        .find(|loaded| loaded.name == model.name)
                        .and_then(|loaded| loaded.context_length)
                        .filter(|limit| *limit > 0)
                    {
                        job.record.metrics.context_limit = Some(limit);
                    }
                }
                if let Some((input, limit)) = job
                    .record
                    .metrics
                    .input_tokens
                    .zip(job.record.metrics.context_limit)
                {
                    job.record.metrics.context_utilization = Some(input as f64 / limit as f64);
                }
                job.record.response = Some(assistant.clone());
                if job.agent.is_some() {
                    context.messages.push(assistant);
                    context.updated_at = Some(Utc::now());
                    context.last_sequence = job.record.meta.sequence;
                    *next_context = Some(context);
                }
                return Ok(());
            }
        }
        Err(Failure::new(
            "upstream_error",
            "Ollama closed its stream before returning a completion frame",
        ))
    }

    async fn finish(
        &self,
        mut job: Job,
        terminal: Result<(), Failure>,
        output: Output,
        context: Option<Context>,
    ) {
        let id = job.record.meta.request_id;
        let live = job.running.progress.lock().unwrap().clone();
        if job.record.response.is_none() && live.frame > 0 {
            job.record.response = Some(live.output);
        }
        job.record.reasoning = job
            .record
            .response
            .as_ref()
            .and_then(|message| message.extra.get("thinking"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if !output.frames.is_empty() || output.upstream_error.is_some() {
            job.record.raw_response =
                Some(json!({"frames":output.frames,"upstream_error":output.upstream_error}));
        }
        job.record.meta.completed_at = Some(Utc::now());
        job.record.metrics.request_ms = Some(milliseconds(job.admitted.elapsed()));
        job.record.error = terminal.err();
        job.record.meta.status = match job.record.error.as_ref().map(|error| error.code.as_str()) {
            None => Status::Completed,
            Some("cancelled") => Status::Cancelled,
            Some(_) => Status::Error,
        };
        let context = context.filter(|_| job.record.meta.status == Status::Completed);
        job.record.context_committed = context.is_some();
        let mut result = json!({
            "type":"done", "request_id":id, "sequence":job.record.meta.sequence,
            "model":job.record.meta.model,"agent_id":job.record.meta.agent_id,"status":job.record.meta.status,
            "message":job.record.response,"metrics":job.record.metrics,"error":job.record.error,
            "done_reason":job.record.done_reason, "model_version":job.record.model_version
        });
        let meta = job.record.meta.clone();
        if let Err(error) = self
            .store()
            .run(move |disk| disk.finish(job.record, context))
            .await
        {
            result["status"] = json!(Status::Error);
            result["error"] = json!(error);
        }
        self.0.admission.lock().await.remove(&id);
        let _ = job.reply.send(result.clone());
        self.emit(json!({"type":"finished","meta":meta,"status":result["status"],"error":result["error"]}));
    }
}

fn unavailable() -> Failure {
    Failure::new("shutting_down", "Zephyr is shutting down")
}

fn ensure_idle(running: &HashMap<Uuid, Running>, id: Uuid) -> Result<(), Failure> {
    if running
        .values()
        .any(|job| job.progress.lock().unwrap().meta.agent_id == Some(id))
    {
        return Err(Failure::new(
            "agent_busy",
            "Wait for or cancel the agent's pending requests first",
        ));
    }
    Ok(())
}

fn milliseconds(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn merge_message(assembled: &mut Message, delta: &Value) {
    if let Some(content) = delta["content"].as_str() {
        assembled.content.push_str(content);
    }
    if let Some(thinking) = delta["thinking"].as_str().filter(|value| !value.is_empty()) {
        if let Value::String(old) = assembled
            .extra
            .entry("thinking".into())
            .or_insert_with(|| json!(""))
        {
            old.push_str(thinking);
        }
    }
    if let Some(calls) = delta["tool_calls"]
        .as_array()
        .filter(|calls| !calls.is_empty())
    {
        let old = assembled
            .extra
            .entry("tool_calls".into())
            .or_insert_with(|| json!([]));
        old.as_array_mut().unwrap().extend(calls.iter().cloned());
    }
}

fn apply_metrics(record: &mut Record, frame: &Value) -> Result<(), Failure> {
    let ms = |key| frame[key].as_u64().map(|ns| ns as f64 / 1_000_000.0);
    let metrics = &mut record.metrics;
    metrics.input_tokens = frame["prompt_eval_count"].as_u64();
    metrics.cached_input_tokens = frame["prompt_eval_cached_count"].as_u64();
    metrics.output_tokens = frame["eval_count"].as_u64();
    metrics.load_ms = ms("load_duration");
    metrics.prompt_ms = ms("prompt_eval_duration");
    metrics.generation_ms = ms("eval_duration");
    metrics.provider_total_ms = ms("total_duration");
    metrics.tokens_per_second = metrics
        .output_tokens
        .zip(metrics.generation_ms)
        .filter(|(_, elapsed)| *elapsed > 0.0)
        .map(|(tokens, elapsed)| tokens as f64 * 1000.0 / elapsed);
    record.done_reason = frame["done_reason"].as_str().map(str::to_owned);
    if record.done_reason.as_deref().is_some_and(|reason| {
        matches!(
            reason,
            "context_length" | "context_limit" | "context_overflow"
        )
    }) {
        return Err(Failure::new(
            "context_limit",
            "The configured context limit was reached",
        ));
    }
    Ok(())
}
