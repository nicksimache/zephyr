use std::{convert::Infallible, io, net::Ipv4Addr, sync::Arc, time::Duration};

use anyhow::{anyhow, Context as _};
use axum::{
    body::{Body, Bytes},
    extract::{rejection::JsonRejection, DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::{net::TcpListener, sync::broadcast, task::JoinHandle};
use tower_http::cors::{AllowOrigin, CorsLayer};
use uuid::Uuid;

use crate::{
    connections::{self, Connections},
    gateway::{Gateway, Submission},
    provider::{self, Ollama, MIN_VERSION, TEST_MODEL},
    store::{self, Disk, Store},
    types::*,
};

pub(crate) const REQUEST_ID: &str = "x-zephyr-request-id";
pub(crate) const LOG_ID: &str = "x-zephyr-log-id";
pub(crate) const BODY_LIMIT: usize = 8 * 1024 * 1024;
const APP_ORIGINS: [&str; 4] = [
    "http://tauri.localhost",
    "https://tauri.localhost",
    "tauri://localhost",
    "http://127.0.0.1:1420",
];

pub struct Server {
    pub gateway: Gateway,
    pub port: u16,
    http: JoinHandle<()>,
    connections: Connections,
}

impl Server {
    pub async fn start() -> anyhow::Result<Self> {
        let root = store::data_root()?;
        let store = tokio::task::spawn_blocking(move || Store::open(root)).await??;
        let settings = store
            .run(Disk::settings)
            .await
            .map_err(|error| anyhow!(error.message))?;
        let port = match std::env::var("ZEPHYR_PORT") {
            Ok(port) => port
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| anyhow!("ZEPHYR_PORT must be an integer between 1 and 65535"))?,
            Err(_) => settings.port,
        };
        let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| format!("Cannot listen on 127.0.0.1:{port}"))?;
        let ollama = Ollama::new()?;
        let gateway = Gateway::launch(store, port, Arc::new(ollama.clone()), ollama);
        let closing = gateway.closing();
        let connections = Connections::restore(gateway.clone()).await;
        let router = routes(gateway.clone(), connections.clone());
        let http = tokio::spawn(async move {
            if let Err(error) = axum::serve(socket, router)
                .with_graceful_shutdown(closing.cancelled_owned())
                .await
            {
                eprintln!("Zephyr HTTP server: {error}");
            }
        });
        Ok(Self {
            gateway,
            port,
            http,
            connections,
        })
    }

    pub async fn stop(self) {
        self.gateway.closing().cancel();
        self.connections.stop().await;
        self.gateway.stop().await;
        let mut http = self.http;
        if tokio::time::timeout(Duration::from_secs(3), &mut http)
            .await
            .is_err()
        {
            http.abort();
        }
    }
}

fn routes(gateway: Gateway, connections: Connections) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(APP_ORIGINS.map(HeaderValue::from_static)))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::CONTENT_TYPE])
        .expose_headers([
            HeaderName::from_static(REQUEST_ID),
            HeaderName::from_static(LOG_ID),
        ]);
    Router::new()
        .route("/health", get(health))
        .route("/settings", get(get_settings).put(save_settings))
        .route("/models", get(models))
        .route("/models/loaded", get(loaded))
        .route("/models/load", post(load_model))
        .route("/models/unload", post(unload_model))
        .route("/models/pull-test", post(pull_test))
        .route("/ollama/start", post(start_ollama))
        .route("/agents", get(agents).post(create_agent))
        .route(
            "/agents/{id}",
            axum::routing::patch(update_agent).delete(delete_agent),
        )
        .route("/agents/{id}/chat", post(stateful))
        .route("/agents/{id}/context", get(context).delete(reset))
        .route("/agents/{id}/history", get(agent_history))
        .route("/agents/{id}/transcript", get(transcript))
        .route("/chat", post(stateless))
        .route("/logs", get(logs))
        .route("/logs/export", get(export_logs))
        .route("/logs/{id}", get(record))
        .route("/usage", get(usage))
        .route("/requests/{id}", delete(cancel))
        .route("/activity", get(activities))
        .route("/events", get(events))
        .with_state(gateway)
        .merge(connections::routes(connections))
        .fallback(|| async { Failure::missing("Endpoint does not exist") })
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(cors)
        .layer(middleware::from_fn(check_origin))
}

async fn check_origin(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .or_else(|| {
            request
                .uri()
                .authority()
                .map(|authority| authority.as_str())
        });
    if host.is_none_or(|host| {
        let name = host.split(':').next().unwrap_or_default();
        name != "127.0.0.1" && !name.eq_ignore_ascii_case("localhost")
    }) {
        return Failure::new("forbidden_origin", "Use the loopback API address").into_response();
    }
    if request.headers().get(header::ORIGIN).is_some_and(|origin| {
        origin
            .to_str()
            .map_or(true, |origin| !APP_ORIGINS.contains(&origin))
    }) {
        return Failure::new("forbidden_origin", "This browser origin cannot use Zephyr")
            .into_response();
    }
    next.run(request).await
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (status(&self), Json(json!({"error":self}))).into_response()
    }
}

fn status(error: &Failure) -> StatusCode {
    match error.code.as_str() {
        "invalid_request" => StatusCode::BAD_REQUEST,
        "forbidden_origin" => StatusCode::FORBIDDEN,
        "pairing_required" => StatusCode::UNAUTHORIZED,
        "not_found" | "model_unavailable" => StatusCode::NOT_FOUND,
        "cancelled" | "agent_busy" => StatusCode::CONFLICT,
        "context_limit" => StatusCode::PAYLOAD_TOO_LARGE,
        "queue_full" => StatusCode::TOO_MANY_REQUESTS,
        "ollama_missing"
        | "ollama_unavailable"
        | "ollama_upgrade_required"
        | "insufficient_memory"
        | "shutting_down" => StatusCode::SERVICE_UNAVAILABLE,
        "upstream_error" | "peer_unreachable" => StatusCode::BAD_GATEWAY,
        "share_bind" => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub(crate) type Payload<T> = Result<Json<T>, JsonRejection>;
pub(crate) type Api<T> = Result<Json<T>, Failure>;

pub(crate) fn body<T>(payload: Payload<T>) -> Result<T, Failure> {
    payload
        .map(|Json(body)| body)
        .map_err(|error| Failure::invalid(error.body_text()))
}

pub(crate) async fn health(State(gateway): State<Gateway>) -> Json<Value> {
    let (version, loaded, (active, queued)) = tokio::join!(
        gateway.version(),
        gateway.installed_models(true),
        gateway.counts()
    );
    let installed = provider::installed_path().is_some() || version.is_ok();
    let ready = version
        .as_ref()
        .is_ok_and(|version| provider::supported(version));
    let connected = version.is_ok();
    Json(json!({
        "status":"ok","port":gateway.port(),"active_requests":active,"queued_requests":queued,
        "loaded_models":loaded.as_ref().map(Vec::len).unwrap_or(0),
        "ollama":{"installed":installed,"connected":connected,"ready":ready,
            "version":version.ok(),"minimum_version":MIN_VERSION,"test_model":TEST_MODEL}
    }))
}

async fn get_settings(State(g): State<Gateway>) -> Api<Settings> {
    g.store().run(Disk::settings).await.map(Json)
}

async fn save_settings(State(g): State<Gateway>, payload: Payload<Settings>) -> Api<Settings> {
    let settings = body(payload)?;
    g.store()
        .run(move |disk| disk.save_settings(settings))
        .await
        .map(Json)
}

pub(crate) async fn models(State(g): State<Gateway>) -> Api<Vec<Model>> {
    g.installed_models(false).await.map(Json)
}
async fn loaded(State(g): State<Gateway>) -> Api<Vec<Model>> {
    g.installed_models(true).await.map(Json)
}

#[derive(Deserialize)]
struct ModelName {
    model: String,
}

async fn load_model(State(g): State<Gateway>, payload: Payload<ModelName>) -> Api<Value> {
    g.residency(body(payload)?.model, true).await.map(Json)
}

async fn unload_model(State(g): State<Gateway>, payload: Payload<ModelName>) -> Api<Value> {
    g.residency(body(payload)?.model, false).await.map(Json)
}

async fn pull_test(State(g): State<Gateway>) -> Api<Value> {
    g.pull_model().await.map(Json)
}

async fn start_ollama(State(g): State<Gateway>) -> Api<Value> {
    g.start_provider()
        .await
        .map(|version| Json(json!({"version":version})))
}

#[derive(Deserialize, Default)]
struct AgentsQuery {
    #[serde(default)]
    include_deleted: bool,
}

async fn agents(State(g): State<Gateway>, Query(query): Query<AgentsQuery>) -> Api<Vec<Value>> {
    let list = g
        .store()
        .run(move |disk| Ok(disk.agents(query.include_deleted)))
        .await?;
    Ok(Json(
        list.into_iter()
            .map(|(agent, status)| {
                let mut value = json!(agent);
                value["last_status"] = json!(status);
                value
            })
            .collect(),
    ))
}

async fn create_agent(
    State(g): State<Gateway>,
    payload: Payload<AgentConfig>,
) -> Result<(StatusCode, Json<Agent>), Failure> {
    Ok((
        StatusCode::CREATED,
        Json(g.create_agent(body(payload)?).await?),
    ))
}

async fn update_agent(
    State(g): State<Gateway>,
    Path(id): Path<Uuid>,
    payload: Payload<AgentPatch>,
) -> Api<Agent> {
    g.update_agent(id, body(payload)?).await.map(Json)
}

async fn delete_agent(
    State(g): State<Gateway>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    g.delete_agent(id).await.map(|()| StatusCode::NO_CONTENT)
}

async fn context(State(g): State<Gateway>, Path(id): Path<Uuid>) -> Api<Context> {
    g.store().run(move |disk| disk.context(id)).await.map(Json)
}

async fn reset(State(g): State<Gateway>, Path(id): Path<Uuid>) -> Api<Context> {
    g.reset(id).await.map(Json)
}

async fn logs(State(g): State<Gateway>, Query(query): Query<LogQuery>) -> Api<Value> {
    g.store()
        .run(move |disk| log_page(disk, query))
        .await
        .map(Json)
}

async fn usage(State(g): State<Gateway>, Query(query): Query<LogQuery>) -> Api<TokenUsage> {
    g.store()
        .run(move |disk| Ok(disk.usage(query)))
        .await
        .map(Json)
}

async fn export_logs(
    State(g): State<Gateway>,
    Query(query): Query<LogQuery>,
) -> Result<Response, Failure> {
    let (path, ranges) = g
        .store()
        .run(move |disk| Ok(disk.export_ranges(query)))
        .await?;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|error| Failure::storage(error.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=zephyr-requests.jsonl",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(export_stream(file, ranges)),
    )
        .into_response())
}

fn export_stream(
    mut file: tokio::fs::File,
    ranges: Vec<(u64, u64)>,
) -> impl futures_util::Stream<Item = io::Result<Bytes>> {
    async_stream::try_stream! {
        for (offset, mut length) in ranges {
            file.seek(io::SeekFrom::Start(offset)).await?;
            while length > 0 {
                let size = length.min(64 * 1024) as usize;
                let mut bytes = vec![0; size];
                file.read_exact(&mut bytes).await?;
                length -= size as u64;
                yield Bytes::from(bytes);
            }
        }
    }
}

async fn agent_history(
    State(g): State<Gateway>,
    Path(id): Path<Uuid>,
    Query(mut query): Query<LogQuery>,
) -> Api<Value> {
    query.agent = Some(id);
    g.store()
        .run(move |disk| {
            disk.agent(id, true)?;
            log_page(disk, query)
        })
        .await
        .map(Json)
}

fn log_page(disk: &mut Disk, query: LogQuery) -> Result<Value, Failure> {
    let full = query.full;
    let page = disk.page(query);
    if !full {
        return Ok(json!(page));
    }
    let items: Vec<_> = page
        .items
        .iter()
        .map(|row| disk.record(row.meta.request_id))
        .collect::<Result<_, _>>()?;
    Ok(json!({"items":items,"next_before":page.next_before}))
}

async fn transcript(
    State(g): State<Gateway>,
    Path(id): Path<Uuid>,
    Query(mut query): Query<LogQuery>,
) -> Api<Value> {
    query.agent = Some(id);
    g.store()
        .run(move |disk| {
            disk.agent(id, true)?;
            let page = disk.page(query);
            let items: Vec<_> = page
                .items
                .iter()
                .map(|row| disk.turn(row.meta.request_id))
                .collect::<Result<_, _>>()?;
            Ok(json!({"items":items,"next_before":page.next_before}))
        })
        .await
        .map(Json)
}

async fn record(State(g): State<Gateway>, Path(id): Path<Uuid>) -> Api<Record> {
    g.store().run(move |disk| disk.record(id)).await.map(Json)
}

async fn cancel(State(g): State<Gateway>, Path(id): Path<Uuid>) -> Result<StatusCode, Failure> {
    g.cancel(id).await.map(|()| StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct ActivityQuery {
    #[serde(default)]
    metadata_only: bool,
}

async fn activities(State(g): State<Gateway>, Query(query): Query<ActivityQuery>) -> Json<Value> {
    let activities = g.activities().await;
    if query.metadata_only {
        return Json(json!(activities
            .into_iter()
            .map(|activity| activity.meta)
            .collect::<Vec<_>>()));
    }
    Json(json!(activities))
}

async fn events(State(g): State<Gateway>, Query(query): Query<ActivityQuery>) -> impl IntoResponse {
    let mut receiver = g.events();
    let closing = g.closing();
    let events = async_stream::stream! {
        let Json(activities) = activities(State(g.clone()), Query(ActivityQuery { metadata_only: query.metadata_only })).await;
        let snapshot = json!({"type":"snapshot", "activities":activities});
        yield Ok::<_, Infallible>(Event::default().data(snapshot.to_string()));
        loop {
            let mut event = tokio::select! {
                _ = closing.cancelled() => break,
                next = receiver.recv() => match next {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => json!({"type":"resync"}),
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            if query.metadata_only {
                match event["type"].as_str() {
                    Some("delta") => continue,
                    Some("queued" | "generating") => {
                        if let Some(activity) = event["activity"].as_object_mut() {
                            activity.remove("messages");
                            activity.remove("output");
                            activity.remove("frame");
                        }
                    }
                    _ => {}
                }
            }
            yield Ok(Event::default().data(event.to_string()));
        }
    };
    Sse::new(events).keep_alive(KeepAlive::default())
}

pub(crate) async fn stateless(
    State(g): State<Gateway>,
    payload: Payload<Value>,
) -> Result<Response, Failure> {
    chat(g, None, body(payload)?).await
}

async fn stateful(
    State(g): State<Gateway>,
    Path(id): Path<Uuid>,
    payload: Payload<Value>,
) -> Result<Response, Failure> {
    chat(g, Some(id), body(payload)?).await
}

async fn chat(g: Gateway, agent: Option<Uuid>, raw: Value) -> Result<Response, Failure> {
    let call: Chat =
        serde_json::from_value(raw.clone()).map_err(|error| Failure::invalid(error.to_string()))?;
    let stream = call.stream;
    let Submission {
        id,
        start,
        responses: mut replies,
        guard,
    } = g.admit(agent, call, raw).await?;
    let mut response = if stream {
        let events = async_stream::stream! {
            let guard = guard;
            yield Ok::<_, Infallible>(line(&start));
            while let Some(event) = replies.recv().await {
                yield Ok(line(&event));
            }
            drop(guard);
        };
        (
            [
                (header::CONTENT_TYPE, "application/x-ndjson"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            Body::from_stream(events),
        )
            .into_response()
    } else {
        let result = replies
            .recv()
            .await
            .ok_or_else(|| Failure::new("upstream_error", "Request finished without a result"))?;
        let error = serde_json::from_value::<Failure>(result["error"].clone()).ok();
        drop(guard);
        (error.as_ref().map_or(StatusCode::OK, status), Json(result)).into_response()
    };
    response.headers_mut().insert(
        HeaderName::from_static(REQUEST_ID),
        HeaderValue::from_str(&id.to_string()).unwrap(),
    );
    Ok(response)
}

fn line(value: &Value) -> Bytes {
    let mut line = value.to_string();
    line.push('\n');
    Bytes::from(line)
}
