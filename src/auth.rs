use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};
use twitch_oauth2::Scope;
use zeroize::Zeroize;

#[derive(Clone)]
pub struct Endpoints {
    pub oauth: String,
    pub helix: String,
    pub websocket: String,
}
impl Default for Endpoints {
    fn default() -> Self {
        Self {
            oauth: "https://id.twitch.tv/oauth2".into(),
            helix: "https://api.twitch.tv/helix".into(),
            websocket: "wss://eventsub.wss.twitch.tv/ws".into(),
        }
    }
}
impl Endpoints {
    pub fn local(base: &str) -> Self {
        Self {
            oauth: base.into(),
            helix: base.into(),
            websocket: base.replacen("http", "ws", 1),
        }
    }
}

#[derive(Clone)]
pub struct Credentials {
    pub(crate) client_id: String,
    client_secret: String,
}
impl Credentials {
    pub fn new(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
        }
    }
}
impl Drop for Credentials {
    fn drop(&mut self) {
        self.client_secret.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    user_id: String,
}
impl Drop for StoredTokens {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}
#[derive(Deserialize)]
struct Validation {
    client_id: String,
    user_id: Option<String>,
    scopes: Vec<String>,
    expires_in: u64,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
}
#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u64,
    expires_in: u64,
}
struct State {
    tokens: StoredTokens,
    expires: Instant,
    validated: Instant,
    active: bool,
}
struct Inner {
    credentials: Credentials,
    path: PathBuf,
    endpoints: Endpoints,
    http: Client,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct AuthManager {
    inner: Arc<Inner>,
}

pub(crate) fn http_client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .context("cannot initialize HTTP client")
}
fn required_scopes() -> [String; 2] {
    [
        Scope::UserReadChat.to_string(),
        Scope::UserWriteChat.to_string(),
    ]
}
fn scope_validation(
    validation: &Validation,
    client: &str,
    expected: Option<&str>,
) -> Result<String> {
    ensure!(
        validation.client_id == client,
        "OAuth client does not match twitch.client_id; run auth again"
    );
    ensure!(
        required_scopes()
            .iter()
            .all(|scope| validation.scopes.contains(scope)),
        "OAuth grant lacks user:read:chat or user:write:chat; run auth again"
    );
    let user = validation
        .user_id
        .as_ref()
        .filter(|s| !s.is_empty())
        .context("OAuth token must belong to a bot user; run auth again")?;
    ensure!(
        expected.is_none_or(|expected| expected == user),
        "OAuth bot identity changed; run auth again for the intended account"
    );
    ensure!(
        validation.expires_in > 0,
        "OAuth token expired; run auth again"
    );
    Ok(user.clone())
}
async fn validate(http: &Client, endpoints: &Endpoints, access: &str) -> Result<Validation> {
    let response = http
        .get(format!("{}/validate", endpoints.oauth))
        .bearer_auth(access)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("OAuth validation connection failed"))?;
    ensure!(
        response.status().is_success(),
        "OAuth validation rejected (HTTP {}); run auth again",
        response.status().as_u16()
    );
    response
        .json()
        .await
        .map_err(|_| anyhow::anyhow!("OAuth validation returned invalid data"))
}
fn persist(path: &Path, tokens: &StoredTokens) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).context("cannot create token directory")?;
    let mut file = tempfile::Builder::new()
        .prefix(".bot-token-")
        .tempfile_in(parent)
        .context("cannot create private token file")?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .context("cannot protect token file")?;
    serde_json::to_writer(file.as_file_mut(), tokens).context("cannot encode token file")?;
    file.flush().context("cannot write token file")?;
    file.as_file()
        .sync_all()
        .context("cannot synchronize token file")?;
    file.persist(path)
        .map_err(|_| anyhow::anyhow!("cannot atomically replace token file"))?;
    std::fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .context("cannot synchronize token directory")?;
    Ok(())
}
impl AuthManager {
    pub async fn load(
        credentials: Credentials,
        path: impl AsRef<Path>,
        endpoints: Endpoints,
    ) -> Result<Self> {
        let path = std::path::absolute(path.as_ref()).context("cannot resolve token path")?;
        let metadata = std::fs::metadata(&path).context("token file missing; run auth first")?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
            "token file must be a private regular file with mode 0600"
        );
        let tokens: StoredTokens =
            serde_json::from_slice(&std::fs::read(&path).context("cannot read token file")?)
                .map_err(|_| anyhow::anyhow!("token file invalid; run auth again"))?;
        ensure!(
            !tokens.access_token.is_empty()
                && !tokens.refresh_token.is_empty()
                && !tokens.user_id.is_empty(),
            "token file incomplete; run auth again"
        );
        let http = http_client()?;
        let manager = Self {
            inner: Arc::new(Inner {
                credentials,
                path,
                endpoints,
                http,
                state: Mutex::new(State {
                    tokens,
                    expires: Instant::now(),
                    validated: Instant::now(),
                    active: true,
                }),
            }),
        };
        let mut state = manager.inner.state.lock().await;
        let response = manager
            .inner
            .http
            .get(format!("{}/validate", manager.inner.endpoints.oauth))
            .bearer_auth(&state.tokens.access_token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OAuth validation connection failed"))?;
        if response.status() == StatusCode::UNAUTHORIZED {
            manager.refresh_locked(&mut state).await?;
        } else {
            ensure!(
                response.status().is_success(),
                "OAuth validation failed (HTTP {})",
                response.status().as_u16()
            );
            let validation: Validation = response
                .json()
                .await
                .map_err(|_| anyhow::anyhow!("OAuth validation returned invalid data"))?;
            scope_validation(
                &validation,
                &manager.inner.credentials.client_id,
                Some(&state.tokens.user_id),
            )?;
            state.expires = Instant::now() + Duration::from_secs(validation.expires_in);
            state.validated = Instant::now();
        }
        drop(state);
        Ok(manager)
    }
    pub async fn authorize(
        credentials: Credentials,
        path: impl AsRef<Path>,
        endpoints: Endpoints,
        display: impl Fn(&str, &str),
    ) -> Result<Self> {
        let http = http_client()?;
        let scopes = required_scopes().join(" ");
        let response = http
            .post(format!("{}/device", endpoints.oauth))
            .form(&[
                ("client_id", credentials.client_id.as_str()),
                ("scopes", scopes.as_str()),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("device authorization connection failed"))?;
        ensure!(
            response.status().is_success(),
            "device authorization rejected (HTTP {})",
            response.status().as_u16()
        );
        let mut device: DeviceCode = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("device authorization returned invalid data"))?;
        ensure!(
            device.expires_in > 0 && !device.device_code.is_empty(),
            "device authorization returned invalid code"
        );
        display(&device.verification_uri, &device.user_code);
        let deadline = Instant::now() + Duration::from_secs(device.expires_in);
        let mut interval = Duration::from_secs(device.interval.max(1));
        let tokens = loop {
            if Instant::now() + interval >= deadline {
                bail!("device authorization expired; run auth again");
            }
            tokio::time::sleep(interval).await;
            let response = http
                .post(format!("{}/token", endpoints.oauth))
                .form(&[
                    ("client_id", credentials.client_id.as_str()),
                    ("client_secret", credentials.client_secret.as_str()),
                    ("scopes", scopes.as_str()),
                    ("device_code", device.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("device token exchange connection failed"))?;
            if response.status().is_success() {
                let token: TokenResponse = response
                    .json()
                    .await
                    .map_err(|_| anyhow::anyhow!("device token exchange returned invalid data"))?;
                ensure!(
                    !token.access_token.is_empty() && !token.refresh_token.is_empty(),
                    "device token exchange returned incomplete tokens"
                );
                let validation = validate(&http, &endpoints, &token.access_token).await?;
                let user = scope_validation(&validation, &credentials.client_id, None)?;
                break StoredTokens {
                    access_token: token.access_token,
                    refresh_token: token.refresh_token,
                    user_id: user,
                };
            }
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let code = body
                .get("error")
                .or_else(|| body.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match code {
                "authorization_pending" => {}
                "slow_down" => interval += Duration::from_secs(5),
                "expired_token" => bail!("device authorization expired; run auth again"),
                "access_denied" => bail!("device authorization denied; run auth again"),
                _ => bail!("device token exchange rejected; run auth again"),
            }
        };
        device.device_code.zeroize();
        persist(path.as_ref(), &tokens)?;
        Self::load(credentials, path, endpoints).await
    }
    pub fn client_id(&self) -> &str {
        &self.inner.credentials.client_id
    }
    pub fn endpoints(&self) -> &Endpoints {
        &self.inner.endpoints
    }
    pub async fn user_id(&self) -> String {
        self.inner.state.lock().await.tokens.user_id.clone()
    }
    pub async fn access_token(&self) -> Result<String> {
        self.maintain().await?;
        Ok(self.inner.state.lock().await.tokens.access_token.clone())
    }
    pub async fn maintain(&self) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        ensure!(state.active, "OAuth session stopped; run auth again");
        if state.expires.saturating_duration_since(Instant::now()) <= Duration::from_secs(60) {
            self.refresh_locked(&mut state).await?;
        } else if state.validated.elapsed() >= Duration::from_secs(3300) {
            state.active = false;
            let result = validate(
                &self.inner.http,
                &self.inner.endpoints,
                &state.tokens.access_token,
            )
            .await?;
            scope_validation(
                &result,
                &self.inner.credentials.client_id,
                Some(&state.tokens.user_id),
            )?;
            state.expires = Instant::now() + Duration::from_secs(result.expires_in);
            state.validated = Instant::now();
            state.active = true;
        }
        Ok(())
    }
    pub async fn refresh_after_rejection(&self, rejected: &str) -> Result<()> {
        let mut state = self.inner.state.lock().await;
        ensure!(state.active, "OAuth session stopped; run auth again");
        if state.tokens.access_token == rejected {
            self.refresh_locked(&mut state).await?;
        }
        Ok(())
    }
    async fn refresh_locked(&self, state: &mut State) -> Result<()> {
        state.active = false;
        let response = self
            .inner
            .http
            .post(format!("{}/token", self.inner.endpoints.oauth))
            .form(&[
                ("client_id", self.inner.credentials.client_id.as_str()),
                (
                    "client_secret",
                    self.inner.credentials.client_secret.as_str(),
                ),
                ("refresh_token", state.tokens.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("OAuth refresh connection failed; run auth again"))?;
        ensure!(
            response.status().is_success(),
            "OAuth refresh rejected (HTTP {}); run auth again",
            response.status().as_u16()
        );
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|_| anyhow::anyhow!("OAuth refresh returned invalid data"))?;
        ensure!(
            !token.access_token.is_empty() && !token.refresh_token.is_empty(),
            "OAuth refresh returned incomplete tokens"
        );
        state.tokens.access_token = token.access_token;
        state.tokens.refresh_token = token.refresh_token;
        // Refresh tokens rotate at the server. Preserve the new pair even when
        // subsequent validation is interrupted; it remains unusable until validated.
        persist(&self.inner.path, &state.tokens)?;
        let validation = validate(
            &self.inner.http,
            &self.inner.endpoints,
            &state.tokens.access_token,
        )
        .await?;
        scope_validation(
            &validation,
            &self.inner.credentials.client_id,
            Some(&state.tokens.user_id),
        )?;
        state.expires = Instant::now() + Duration::from_secs(validation.expires_in);
        state.validated = Instant::now();
        state.active = true;
        Ok(())
    }
}
