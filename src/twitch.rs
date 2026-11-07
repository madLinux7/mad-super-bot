use crate::auth::{AuthManager, http_client};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Method, Response, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpStream,
    sync::{Mutex, mpsc, watch},
    time::{Instant, timeout, timeout_at},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use twitch_api::helix::chat::{SendChatMessageBody, SendChatMessageResponse};

pub enum SendOutcome {
    Sent { cooldown: Duration },
    Rejected { code: String, cooldown: Duration },
    RateLimited { retry_after: Duration },
    Unauthorized { rejected_token: String },
    Uncertain,
    AuthorizationFailed,
}
impl std::fmt::Debug for SendOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sent { cooldown } => f.debug_struct("Sent").field("cooldown", cooldown).finish(),
            Self::Rejected { code, cooldown } => f
                .debug_struct("Rejected")
                .field("code", code)
                .field("cooldown", cooldown)
                .finish(),
            Self::RateLimited { retry_after } => f
                .debug_struct("RateLimited")
                .field("retry_after", retry_after)
                .finish(),
            Self::Unauthorized { .. } => f.write_str("Unauthorized"),
            Self::Uncertain => f.write_str("Uncertain"),
            Self::AuthorizationFailed => f.write_str("AuthorizationFailed"),
        }
    }
}
#[derive(Clone)]
pub struct Api {
    auth: AuthManager,
    channel_id: String,
    http: Client,
    send_pause: Arc<Mutex<Instant>>,
    api_pause: Arc<Mutex<Instant>>,
}
impl Api {
    pub fn new(auth: AuthManager, channel_id: impl Into<String>) -> Result<Self> {
        Self::with_http(auth, channel_id, http_client()?)
    }
    pub fn with_http(
        auth: AuthManager,
        channel_id: impl Into<String>,
        http: Client,
    ) -> Result<Self> {
        Ok(Self {
            auth,
            channel_id: channel_id.into(),
            http,
            send_pause: Arc::new(Mutex::new(Instant::now())),
            api_pause: Arc::new(Mutex::new(Instant::now())),
        })
    }
    pub fn auth(&self) -> &AuthManager {
        &self.auth
    }
    pub async fn rate_limit_delay(&self) -> Duration {
        self.send_pause
            .lock()
            .await
            .saturating_duration_since(Instant::now())
    }
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Response> {
        for attempt in 0..2 {
            let wait = self
                .api_pause
                .lock()
                .await
                .saturating_duration_since(Instant::now());
            tokio::time::sleep(wait).await;
            let token = self.auth.access_token().await?;
            let mut request = self
                .http
                .request(
                    method.clone(),
                    format!("{}{}", self.auth.endpoints().helix, path),
                )
                .bearer_auth(&token)
                .header("Client-Id", self.auth.client_id());
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = request
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("Twitch API connection failed"))?;
            let delay = rate_delay(&response, false);
            if !delay.is_zero() {
                *self.api_pause.lock().await = Instant::now() + delay;
            }
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                self.auth.refresh_after_rejection(&token).await?;
                continue;
            }
            ensure!(
                response.status().is_success(),
                "Twitch API rejected request (HTTP {})",
                response.status().as_u16()
            );
            return Ok(response);
        }
        bail!("Twitch authorization rejected; run auth again")
    }
    pub async fn resolve_channel(auth: &AuthManager, login: &str) -> Result<String> {
        ensure!(
            !login.is_empty()
                && login
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "twitch.channel must be a Twitch login"
        );
        let api = Self::new(auth.clone(), "")?;
        let response: Value = api
            .request(Method::GET, &format!("/users?login={login}"), None)
            .await?
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Twitch users response invalid"))?;
        response["data"]
            .as_array()
            .and_then(|v| v.first())
            .and_then(|v| v["id"].as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .context("twitch.channel was not found")
    }
    pub async fn get_streams(&self) -> Result<bool> {
        let response: Value = self
            .request(
                Method::GET,
                &format!("/streams?user_id={}", self.channel_id),
                None,
            )
            .await?
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("Twitch streams response invalid"))?;
        Ok(!response["data"]
            .as_array()
            .context("Twitch streams response missing data")?
            .is_empty())
    }
    pub async fn subscribe(&self, session: &str) -> Result<()> {
        let bot_id = self.auth.user_id().await;
        for kind in ["channel.chat.message", "stream.online", "stream.offline"] {
            let mut condition = json!({"broadcaster_user_id":self.channel_id});
            if kind == "channel.chat.message" {
                condition["user_id"] = json!(bot_id);
            }
            self.request(Method::POST,"/eventsub/subscriptions",Some(json!({"type":kind,"version":"1","condition":condition,"transport":{"method":"websocket","session_id":session}}))).await?;
        }
        Ok(())
    }
    pub async fn send_chat(&self, text: &str) -> SendOutcome {
        let token = match self.auth.access_token().await {
            Ok(t) => t,
            Err(_) => return SendOutcome::AuthorizationFailed,
        };
        let bot_id = self.auth.user_id().await;
        self.send_prepared(&token, &bot_id, text).await
    }

    /// Send once after the owner rechecks configuration with a prepared token.
    pub async fn send_prepared(&self, token: &str, bot_id: &str, text: &str) -> SendOutcome {
        // The SDK omits for_source_only for user-token requests.
        let body = SendChatMessageBody::new(self.channel_id.as_str(), bot_id, text);
        let response = match self
            .http
            .post(format!("{}/chat/messages", self.auth.endpoints().helix))
            .bearer_auth(token)
            .header("Client-Id", self.auth.client_id())
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return SendOutcome::Uncertain,
        };
        let cooldown = rate_delay(
            &response,
            response.status() == StatusCode::TOO_MANY_REQUESTS,
        );
        if !cooldown.is_zero() {
            *self.send_pause.lock().await = Instant::now() + cooldown;
        }
        match response.status() {
            StatusCode::UNAUTHORIZED => SendOutcome::Unauthorized {
                rejected_token: token.to_owned(),
            },
            StatusCode::TOO_MANY_REQUESTS => SendOutcome::RateLimited {
                retry_after: cooldown,
            },
            status if status.is_success() => {
                #[derive(Deserialize)]
                struct SentResponse {
                    data: Vec<SendChatMessageResponse>,
                }
                match response.json::<SentResponse>().await {
                    Ok(mut response) if response.data.len() == 1 => {
                        let response = response.data.remove(0);
                        if response.is_sent {
                            SendOutcome::Sent { cooldown }
                        } else {
                            let code = response
                                .drop_reason
                                .and_then(|r| serde_json::to_value(r.code).ok())
                                .and_then(|v| v.as_str().map(str::to_owned))
                                .filter(|s| {
                                    s.len() <= 64
                                        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                                })
                                .unwrap_or_else(|| "unknown_drop".into());
                            SendOutcome::Rejected { code, cooldown }
                        }
                    }
                    _ => SendOutcome::Uncertain,
                }
            }
            status if status.is_client_error() => SendOutcome::Rejected {
                code: format!("http_{}", status.as_u16()),
                cooldown,
            },
            _ => SendOutcome::Uncertain,
        }
    }
}
fn rate_delay(response: &Response, rate_limited: bool) -> Duration {
    let headers = response.headers();
    let seconds = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let exhausted = seconds("Ratelimit-Remaining") == Some(0);
    if !rate_limited && !exhausted {
        return Duration::ZERO;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let reset = seconds("Ratelimit-Reset")
        .map(|reset| reset.saturating_sub(now).saturating_add(1))
        .unwrap_or(1);
    Duration::from_secs(seconds("Retry-After").unwrap_or(0).max(reset).max(1))
}

pub enum TwitchEvent {
    Ready,
    Disconnected,
    Chat {
        user_id: String,
        login: String,
        text: String,
    },
    Online,
    Offline,
    Fatal,
}
impl std::fmt::Debug for TwitchEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ready => "Ready",
            Self::Disconnected => "Disconnected",
            Self::Chat { .. } => "Chat",
            Self::Online => "Online",
            Self::Offline => "Offline",
            Self::Fatal => "Fatal",
        })
    }
}
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
struct Connection {
    socket: Socket,
    session: String,
    keepalive: Duration,
    last_activity: Instant,
}
pub struct EventSub {
    api: Api,
}
impl EventSub {
    pub fn new(api: Api) -> Self {
        Self { api }
    }
    pub async fn run(self, tx: mpsc::Sender<TwitchEvent>, mut shutdown: watch::Receiver<bool>) {
        let mut seen = Dedup::default();
        let mut backoff = 1;
        loop {
            if *shutdown.borrow() {
                break;
            }
            let result = tokio::select! {
                _=shutdown.changed()=>break,
                result=self.session(&tx,&mut seen)=>result
            };
            if result
                .as_ref()
                .is_err_and(|e| e.downcast_ref::<Revoked>().is_some())
            {
                break;
            }
            if result.is_err() {
                tracing::warn!("EventSub disconnected; reconnecting");
            }
            tokio::select! {
                _ = shutdown.changed() => break,
                result = tx.send(TwitchEvent::Disconnected) => { if result.is_err() { break; } }
            }
            tokio::select! {_=shutdown.changed()=>break,_=tokio::time::sleep(Duration::from_secs(backoff))=>{}}
            backoff = (backoff * 2).min(30);
        }
    }
    async fn session(&self, tx: &mpsc::Sender<TwitchEvent>, seen: &mut Dedup) -> Result<()> {
        let mut connection = open_welcome(&self.api.auth.endpoints().websocket).await?;
        timeout(
            connection.keepalive,
            self.api.subscribe(&connection.session),
        )
        .await
        .context("EventSub subscription deadline expired")??;
        tx.send(TwitchEvent::Ready)
            .await
            .context("event receiver closed")?;
        loop {
            let value = next_value(&mut connection).await?;
            match kind(&value) {
                "session_reconnect" => {
                    let url = value["payload"]["session"]["reconnect_url"]
                        .as_str()
                        .context("EventSub reconnect URL missing")?;
                    ensure!(
                        valid_reconnect_url(&self.api.auth.endpoints().websocket, url),
                        "EventSub reconnect URL invalid"
                    );
                    let new_connection = open_welcome(url);
                    tokio::pin!(new_connection);
                    loop {
                        tokio::select! {
                            new=&mut new_connection=>{
                                let new=new?;
                                let _=timeout(Duration::from_secs(1),connection.socket.close(None)).await;
                                connection=new;break
                            }
                            old=next_value(&mut connection)=>{
                                let old=old?;
                                self.notification(&old,tx,seen).await?;
                            }
                        }
                    }
                }
                _ => self.notification(&value, tx, seen).await?,
            }
        }
    }
    async fn notification(
        &self,
        value: &Value,
        tx: &mpsc::Sender<TwitchEvent>,
        seen: &mut Dedup,
    ) -> Result<()> {
        match kind(value) {
            "session_keepalive" => Ok(()),
            "revocation" => {
                let status = value["payload"]["subscription"]["status"]
                    .as_str()
                    .unwrap_or("");
                if matches!(status, "authorization_revoked" | "user_removed") {
                    let _ = tx.send(TwitchEvent::Fatal).await;
                    return Err(anyhow::Error::new(Revoked));
                }
                bail!("EventSub subscription revoked")
            }
            "notification" => {
                let id = value["metadata"]["message_id"]
                    .as_str()
                    .context("EventSub notification ID missing")?;
                if !seen.insert(id, Instant::now()) {
                    return Ok(());
                }
                let event = &value["payload"]["event"];
                let event = match value["metadata"]["subscription_type"]
                    .as_str()
                    .unwrap_or("")
                {
                    "channel.chat.message" => {
                        let field = |name: &str| {
                            event[name]
                                .as_str()
                                .map(str::to_owned)
                                .context("EventSub chat event missing user field")
                        };
                        TwitchEvent::Chat {
                            user_id: field("chatter_user_id")?,
                            login: field("chatter_user_login")?,
                            text: event["message"]["text"]
                                .as_str()
                                .context("EventSub chat event missing message")?
                                .into(),
                        }
                    }
                    "stream.online" => TwitchEvent::Online,
                    "stream.offline" => TwitchEvent::Offline,
                    _ => return Ok(()),
                };
                tx.send(event).await.context("event receiver closed")
            }
            _ => Ok(()),
        }
    }
}
fn kind(value: &Value) -> &str {
    value["metadata"]["message_type"].as_str().unwrap_or("")
}
fn valid_reconnect_url(original: &str, url: &str) -> bool {
    match (reqwest::Url::parse(original), reqwest::Url::parse(url)) {
        (Ok(original), Ok(url)) => {
            url.scheme() == original.scheme()
                && url.host_str() == original.host_str()
                && url.port_or_known_default() == original.port_or_known_default()
                && url.username().is_empty()
                && url.password().is_none()
        }
        _ => false,
    }
}
async fn open_welcome(url: &str) -> Result<Connection> {
    timeout(Duration::from_secs(10), async {
        let (socket, _) = connect_async(url)
            .await
            .map_err(|_| anyhow::anyhow!("EventSub connection failed"))?;
        let mut connection = Connection {
            socket,
            session: String::new(),
            keepalive: Duration::from_secs(10),
            last_activity: Instant::now(),
        };
        let value = next_value(&mut connection).await?;
        ensure!(
            kind(&value) == "session_welcome",
            "EventSub welcome missing"
        );
        connection.session = value["payload"]["session"]["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("EventSub session ID missing")?
            .into();
        connection.keepalive = Duration::from_secs(
            value["payload"]["session"]["keepalive_timeout_seconds"]
                .as_u64()
                .unwrap_or(10)
                .clamp(1, 600),
        );
        Ok(connection)
    })
    .await
    .context("EventSub welcome timed out")?
}
async fn next_value(connection: &mut Connection) -> Result<Value> {
    loop {
        let message = timeout_at(
            connection.last_activity + connection.keepalive,
            connection.socket.next(),
        )
        .await
        .context("EventSub keepalive timed out")?
        .context("EventSub closed")?
        .map_err(|_| anyhow::anyhow!("EventSub frame failed"))?;
        match message {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|_| anyhow::anyhow!("EventSub JSON invalid"))?;
                if matches!(
                    kind(&value),
                    "session_welcome" | "notification" | "session_keepalive"
                ) {
                    connection.last_activity = Instant::now();
                }
                return Ok(value);
            }
            Message::Ping(payload) => {
                connection
                    .socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|_| anyhow::anyhow!("EventSub pong failed"))?;
            }
            Message::Close(_) => bail!("EventSub closed"),
            _ => {}
        }
    }
}
#[derive(Debug)]
struct Revoked;
impl std::fmt::Display for Revoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventSub authorization revoked; run auth again")
    }
}
impl std::error::Error for Revoked {}
#[derive(Default)]
struct Dedup {
    ids: HashSet<String>,
    order: VecDeque<(String, Instant)>,
}
impl Dedup {
    fn insert(&mut self, id: &str, now: Instant) -> bool {
        while self
            .order
            .front()
            .is_some_and(|(_, time)| now.duration_since(*time) >= Duration::from_secs(600))
        {
            if let Some((id, _)) = self.order.pop_front() {
                self.ids.remove(&id);
            }
        }
        if self.ids.contains(id) {
            return false;
        }
        while self.ids.len() >= 4096 {
            if let Some((old, _)) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        self.ids.insert(id.into());
        self.order.push_back((id.into(), now));
        true
    }
}
