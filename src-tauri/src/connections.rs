use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{header, HeaderName, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum_server::{
    tls_rustls::{from_tcp_rustls, RustlsConfig},
    Handle,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};
use uuid::Uuid;

use crate::{
    api::{self, body, Api, Payload, REQUEST_ID},
    gateway::Gateway,
    store::Disk,
    types::Failure,
};

const INVITE_PREFIX: &str = "zephyr1.";
const DEFAULT_SHARE_PORT: u16 = 47834;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NetworkSettings {
    share: Option<Share>,
    #[serde(default)]
    peers: BTreeMap<Uuid, Peer>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Share {
    invite: Invite,
    private_key: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Invite {
    address: SocketAddrV4,
    certificate: String,
    token: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct Peer {
    name: String,
    invite: Invite,
}

#[derive(Serialize)]
struct PeerInfo {
    id: Uuid,
    name: String,
    address: SocketAddrV4,
}

impl Peer {
    fn info(&self, id: Uuid) -> PeerInfo {
        PeerInfo {
            id,
            name: self.name.clone(),
            address: self.invite.address,
        }
    }
}

#[derive(Clone)]
pub struct Connections {
    gateway: Gateway,
    state: Arc<Mutex<Network>>,
}

#[derive(Default)]
struct Network {
    data: NetworkSettings,
    clients: BTreeMap<Uuid, Client>,
    listener: Option<ShareServer>,
    error: Option<Failure>,
    load_error: Option<Failure>,
}

type ShareToken = Arc<RwLock<String>>;

struct ShareServer {
    handle: Handle<SocketAddr>,
    task: JoinHandle<()>,
    token: ShareToken,
}

impl ShareServer {
    async fn start(gateway: Gateway, share: &Share) -> Result<Self, Failure> {
        let address = share.invite.address;
        let socket = TcpListener::bind(address)
            .await
            .and_then(TcpListener::into_std)
            .map_err(|error| {
                Failure::new(
                    "share_bind",
                    format!("Could not share on {address}: {error}"),
                )
            })?;
        let tls = RustlsConfig::from_pem(
            share.invite.certificate.as_bytes().to_vec(),
            share.private_key.as_bytes().to_vec(),
        )
        .await
        .map_err(|error| Failure::new("share_tls", error.to_string()))?;
        let handle = Handle::new();
        let server = from_tcp_rustls(socket, tls)
            .map_err(|error| Failure::new("share_bind", error.to_string()))?
            .handle(handle.clone());
        let token = Arc::new(RwLock::new(share.invite.token.clone()));
        let router = Router::new()
            .route("/health", get(api::health))
            .route("/models", get(api::models))
            .route("/chat", post(api::stateless))
            .layer(axum::extract::DefaultBodyLimit::max(api::BODY_LIMIT))
            .layer(middleware::from_fn_with_state(token.clone(), require_token))
            .with_state(gateway);
        let task = tokio::spawn(async move {
            if let Err(error) = server.serve(router.into_make_service()).await {
                eprintln!("Zephyr model sharing: {error}");
            }
        });
        Ok(Self {
            handle,
            task,
            token,
        })
    }

    async fn stop(mut self) {
        self.handle.shutdown();
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }

    fn active(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Connections {
    pub async fn restore(gateway: Gateway) -> Self {
        let mut network = Network::default();
        match gateway.store().run(Disk::network_settings).await {
            Ok(data) => network.data = data,
            Err(error) => network.load_error = Some(error),
        }
        if let Some(share) = &network.data.share {
            let result = match validate_address(share.invite.address) {
                Ok(()) => ShareServer::start(gateway.clone(), share).await,
                Err(error) => Err(error),
            };
            match result {
                Ok(server) => network.listener = Some(server),
                Err(error) => network.error = Some(error),
            }
        }
        for (id, peer) in &network.data.peers {
            if let Ok(client) = peer.invite.client() {
                network.clients.insert(*id, client);
            }
        }
        Self {
            gateway,
            state: Arc::new(Mutex::new(network)),
        }
    }

    pub async fn stop(&self) {
        if let Some(listener) = self.state.lock().await.listener.take() {
            listener.stop().await;
        }
    }

    async fn writable(&self) -> Result<tokio::sync::MutexGuard<'_, Network>, Failure> {
        let network = self.state.lock().await;
        if self.gateway.closing().is_cancelled() {
            return Err(Failure::new("shutting_down", "Zephyr is shutting down"));
        }
        if let Some(error) = &network.load_error {
            return Err(error.clone());
        }
        Ok(network)
    }

    async fn save(&self, data: &NetworkSettings) -> Result<(), Failure> {
        let data = data.clone();
        self.gateway
            .store()
            .run(move |disk| disk.save_network_settings(data))
            .await
    }

    async fn send(
        &self,
        id: Uuid,
        route: PeerRoute,
        body: Option<Value>,
    ) -> Result<Response, Failure> {
        let (invite, client) = {
            let state = self.state.lock().await;
            let peer = state
                .data
                .peers
                .get(&id)
                .ok_or_else(|| Failure::missing("Paired computer does not exist"))?;
            let client = state.clients.get(&id).ok_or_else(|| {
                Failure::new(
                    "peer_unreachable",
                    "The saved pairing is invalid. Import a fresh pairing code.",
                )
            })?;
            (peer.invite.clone(), client.clone())
        };
        let upstream = invite.send(&client, route, body).await?;
        let mut response = Response::builder().status(upstream.status());
        for name in [header::CONTENT_TYPE, HeaderName::from_static(REQUEST_ID)] {
            if let Some(value) = upstream.headers().get(&name) {
                response = response.header(name, value);
            }
        }
        Ok(response
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from_stream(upstream.bytes_stream()))
            .unwrap())
    }
}

impl Share {
    fn new(address: SocketAddrV4) -> Result<Self, Failure> {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![address.ip().to_string()])
                .map_err(|error| Failure::new("share_tls", error.to_string()))?;
        Ok(Self {
            invite: Invite {
                address,
                certificate: cert.pem(),
                token: new_token(),
            },
            private_key: signing_key.serialize_pem(),
        })
    }
}

impl Invite {
    fn encode(&self) -> Result<String, Failure> {
        let bytes =
            serde_json::to_vec(self).map_err(|error| Failure::storage(error.to_string()))?;
        Ok(format!("{INVITE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
    }

    fn decode(code: &str) -> Result<Self, Failure> {
        let code = code.trim();
        let invalid = || Failure::invalid("Paste a complete Zephyr pairing code");
        if code.len() > 12_000 {
            return Err(invalid());
        }
        let encoded = code.strip_prefix(INVITE_PREFIX).ok_or_else(invalid)?;
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
        let invite: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        validate_address(invite.address)?;
        if invite.token.len() != 64 || !invite.token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        Ok(invite)
    }

    fn client(&self) -> Result<Client, Failure> {
        validate_address(self.address)?;
        let certificate = reqwest::Certificate::from_pem(self.certificate.as_bytes())
            .map_err(|_| Failure::invalid("The pairing code has an invalid certificate"))?;
        Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| {
                Failure::invalid("The pairing code could not configure a trusted connection")
            })
    }

    async fn send(
        &self,
        client: &Client,
        route: PeerRoute,
        body: Option<Value>,
    ) -> Result<reqwest::Response, Failure> {
        let (method, path) = match route {
            PeerRoute::Health => (Method::GET, "/health"),
            PeerRoute::Models => (Method::GET, "/models"),
            PeerRoute::Chat => (Method::POST, "/chat"),
        };
        let request = client
            .request(method, format!("https://{}{}", self.address, path))
            .bearer_auth(&self.token);
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request.timeout(Duration::from_secs(5))
        };
        request.send().await.map_err(|_| Failure::new("peer_unreachable",
            format!("Cannot verify or reach Zephyr at {}. Check its sharing address, network and pairing code.", self.address)))
    }
}

#[derive(Clone, Copy)]
enum PeerRoute {
    Health,
    Models,
    Chat,
}

fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn private_address(ip: Ipv4Addr) -> bool {
    let [first, second, _, _] = ip.octets();
    ip.is_private() || ip.is_loopback() || (first == 100 && (64..=127).contains(&second))
}

fn validate_address(address: SocketAddrV4) -> Result<(), Failure> {
    if address.port() == 0 || !private_address(*address.ip()) {
        return Err(Failure::invalid(
            "Choose a private IPv4 address and a port between 1 and 65535",
        ));
    }
    Ok(())
}

async fn require_token(State(token): State<ShareToken>, request: Request, next: Next) -> Response {
    if request.headers().contains_key(header::ORIGIN) {
        return Failure::new("forbidden_origin", "Use a paired Zephyr client").into_response();
    }
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, value)| value.as_bytes());
    let allowed =
        supplied.is_some_and(|value| bool::from(value.ct_eq(token.read().unwrap().as_bytes())));
    if !allowed {
        let mut response = Failure::new(
            "pairing_required",
            "A valid Zephyr pairing code is required",
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    next.run(request).await
}

pub fn routes(connections: Connections) -> Router {
    Router::new()
        .route("/connections", get(view))
        .route(
            "/connections/share",
            axum::routing::put(enable_share).delete(disable_share),
        )
        .route("/connections/share/invite", post(pairing_code))
        .route("/connections/share/rotate", post(rotate))
        .route("/peers", post(add_peer))
        .route("/peers/{id}", axum::routing::delete(remove_peer))
        .route("/peers/{id}/health", get(peer_health))
        .route("/peers/{id}/models", get(peer_models))
        .route("/peers/{id}/chat", post(peer_chat))
        .with_state(connections)
}

async fn view(State(connections): State<Connections>) -> Json<Value> {
    let interfaces = tokio::task::spawn_blocking(|| {
        let mut entries = if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|interface| match interface.ip() {
                IpAddr::V4(ip) if private_address(ip) && !ip.is_loopback() => {
                    Some((interface.name, ip))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries.dedup();
        entries
            .into_iter()
            .map(|(name, ip)| json!({"name":name,"ip":ip}))
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();

    let network = connections.state.lock().await;
    let peers = network
        .data
        .peers
        .iter()
        .map(|(id, peer)| peer.info(*id))
        .collect::<Vec<_>>();
    Json(json!({
        "share": {
            "address":network.data.share.as_ref().map(|share| share.invite.address),
            "active":network.listener.as_ref().is_some_and(ShareServer::active),
            "error":network.error.as_ref().or(network.load_error.as_ref())
        },
        "peers":peers,
        "interfaces":interfaces,
        "default_port":DEFAULT_SHARE_PORT
    }))
}

#[derive(Deserialize)]
struct ShareConfig {
    address: SocketAddrV4,
}

async fn enable_share(
    State(connections): State<Connections>,
    payload: Payload<ShareConfig>,
) -> Api<Value> {
    let address = body(payload)?.address;
    validate_address(address)?;
    let mut network = connections.writable().await?;
    let previous = network
        .data
        .share
        .as_ref()
        .filter(|share| share.invite.address == address);
    if previous.is_some() && network.listener.as_ref().is_some_and(ShareServer::active) {
        return Ok(Json(json!({"address":address})));
    }
    let share = match previous {
        Some(share) => share.clone(),
        None => Share::new(address)?,
    };
    let listener = ShareServer::start(connections.gateway.clone(), &share).await?;
    let mut data = network.data.clone();
    data.share = Some(share);
    if let Err(error) = connections.save(&data).await {
        listener.stop().await;
        return Err(error);
    }
    network.data = data;
    network.error = None;
    if let Some(old) = network.listener.replace(listener) {
        old.stop().await;
    }
    Ok(Json(json!({"address":address})))
}

async fn disable_share(State(connections): State<Connections>) -> Result<StatusCode, Failure> {
    let mut network = connections.writable().await?;
    let mut data = network.data.clone();
    data.share = None;
    connections.save(&data).await?;
    network.data = data;
    network.error = None;
    if let Some(listener) = network.listener.take() {
        listener.stop().await;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn pairing_code(State(connections): State<Connections>) -> Result<Response, Failure> {
    let network = connections.state.lock().await;
    let share = network
        .data
        .share
        .as_ref()
        .filter(|_| network.listener.as_ref().is_some_and(ShareServer::active))
        .ok_or_else(|| Failure::missing("Start model sharing first"))?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({"code":share.invite.encode()?})),
    )
        .into_response())
}

async fn rotate(State(connections): State<Connections>) -> Result<StatusCode, Failure> {
    let mut network = connections.writable().await?;
    let mut data = network.data.clone();
    let share = data
        .share
        .as_mut()
        .ok_or_else(|| Failure::missing("Start model sharing first"))?;
    let token = new_token();
    share.invite.token = token.clone();
    connections.save(&data).await?;
    if let Some(listener) = &network.listener {
        *listener.token.write().unwrap() = token;
    }
    network.data = data;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PairInput {
    name: String,
    code: String,
}

async fn add_peer(
    State(connections): State<Connections>,
    payload: Payload<PairInput>,
) -> Api<PeerInfo> {
    let PairInput { name, code } = body(payload)?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(Failure::invalid("Computer name must be 1–80 characters"));
    }
    let invite = Invite::decode(&code)?;
    let client = invite.client()?;
    let response = invite.send(&client, PeerRoute::Health, None).await?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(Failure::new(
            "pairing_required",
            "This code was revoked. Copy a current code from the host.",
        ));
    }
    if !response.status().is_success() {
        return Err(Failure::new(
            "peer_unreachable",
            "The host rejected the pairing check",
        ));
    }
    let health: Value = response.json().await.map_err(|_| {
        Failure::new(
            "peer_unreachable",
            "The host did not return a Zephyr health response",
        )
    })?;
    if health["status"] != "ok"
        || health["ollama"]
            .get("ready")
            .and_then(Value::as_bool)
            .is_none()
    {
        return Err(Failure::new(
            "peer_unreachable",
            "The host did not return a Zephyr health response",
        ));
    }
    let mut network = connections.writable().await?;
    let mut data = network.data.clone();
    let id = data
        .peers
        .iter()
        .find(|(_, peer)| peer.invite.address == invite.address)
        .map(|(id, _)| *id)
        .unwrap_or_else(Uuid::new_v4);
    let peer = Peer {
        name: name.into(),
        invite,
    };
    let info = peer.info(id);
    data.peers.insert(id, peer);
    connections.save(&data).await?;
    network.clients.insert(id, client);
    network.data = data;
    Ok(Json(info))
}

async fn remove_peer(
    State(connections): State<Connections>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, Failure> {
    let mut network = connections.writable().await?;
    let mut data = network.data.clone();
    data.peers
        .remove(&id)
        .ok_or_else(|| Failure::missing("Paired computer does not exist"))?;
    connections.save(&data).await?;
    network.clients.remove(&id);
    network.data = data;
    Ok(StatusCode::NO_CONTENT)
}

async fn peer_health(
    State(connections): State<Connections>,
    Path(id): Path<Uuid>,
) -> Result<Response, Failure> {
    connections.send(id, PeerRoute::Health, None).await
}

async fn peer_models(
    State(connections): State<Connections>,
    Path(id): Path<Uuid>,
) -> Result<Response, Failure> {
    connections.send(id, PeerRoute::Models, None).await
}

async fn peer_chat(
    State(connections): State<Connections>,
    Path(id): Path<Uuid>,
    payload: Payload<Value>,
) -> Result<Response, Failure> {
    connections
        .send(id, PeerRoute::Chat, Some(body(payload)?))
        .await
}
