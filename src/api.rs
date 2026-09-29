use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use rand::{Rng, distr::Alphanumeric};
use reqwest::{Client, StatusCode};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::models::{AppSettings, BeatmapSearchResponse, OAuthTokenResponse, OsuUser};
use crate::paths::DataPaths;
use crate::storage;

const BASE_URL: &str = "https://osu.ppy.sh";
const REDIRECT_URI: &str = "http://localhost:7270/callback";

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("request cancelled")]
    Cancelled,
    #[error("osu! authentication failed")]
    Authentication,
    #[error("osu! returned HTTP {0}")]
    Http(StatusCode),
    #[error("OAuth callback timed out")]
    OAuthTimeout,
    #[error("OAuth callback was rejected: {0}")]
    OAuthRejected(String),
    #[error("could not listen on localhost:7270: {0}")]
    OAuthListener(std::io::Error),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not save account settings: {0}")]
    Storage(#[from] anyhow::Error),
}

#[derive(Debug, Default)]
struct TokenState {
    access_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    is_user: bool,
}

impl TokenState {
    fn valid_token(&self) -> Option<&str> {
        let valid = self.expires_at.is_some_and(|expiry| Utc::now() < expiry);
        valid.then_some(self.access_token.as_deref()).flatten()
    }
}

#[derive(Clone)]
pub struct OsuApiClient {
    client: Client,
    settings: Arc<RwLock<AppSettings>>,
    settings_version: Arc<AtomicU64>,
    token: Arc<tokio::sync::Mutex<TokenState>>,
    paths: DataPaths,
}

impl OsuApiClient {
    pub fn new(settings: AppSettings, paths: DataPaths) -> Result<Self, ApiError> {
        let token = if settings
            .user_token_expiry
            .is_some_and(|expiry| Utc::now() < expiry)
            && settings
                .user_access_token
                .as_ref()
                .is_some_and(|token| !token.is_empty())
        {
            TokenState {
                access_token: settings.user_access_token.clone(),
                expires_at: settings.user_token_expiry,
                is_user: true,
            }
        } else {
            TokenState::default()
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(concat!(
                "OsuBmDownloader/",
                env!("CARGO_PKG_VERSION"),
                "-rust"
            ))
            .build()?;

        Ok(Self {
            client,
            settings: Arc::new(RwLock::new(settings)),
            settings_version: Arc::new(AtomicU64::new(0)),
            token: Arc::new(tokio::sync::Mutex::new(token)),
            paths,
        })
    }

    pub fn settings(&self) -> AppSettings {
        self.settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn settings_version(&self) -> u64 {
        self.settings_version.load(Ordering::Acquire)
    }

    fn store_settings(&self, settings: AppSettings) {
        *self
            .settings
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings;
        self.settings_version.fetch_add(1, Ordering::AcqRel);
    }

    pub async fn replace_settings(&self, settings: AppSettings) {
        let credentials_changed = {
            let current = self
                .settings
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            current.client_id != settings.client_id
                || current.client_secret != settings.client_secret
        };
        self.store_settings(settings);
        if credentials_changed {
            *self.token.lock().await = TokenState::default();
        }
    }

    pub fn update_preferences(
        &self,
        prefer_no_video: bool,
        auto_install: bool,
    ) -> anyhow::Result<()> {
        let settings = {
            let mut settings = self
                .settings
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            settings.prefer_no_video = prefer_no_video;
            settings.auto_install = auto_install;
            self.settings_version.fetch_add(1, Ordering::AcqRel);
            settings.clone()
        };
        storage::save_settings(&self.paths, &settings)
    }

    pub async fn authenticate(&self) -> Result<(), ApiError> {
        let mut token = self.token.lock().await;
        if token.valid_token().is_some() {
            return Ok(());
        }
        if token.is_user {
            *token = TokenState::default();
            drop(token);
            if let Err(error) = self.clear_stale_supporter() {
                tracing::warn!(%error, "could not persist expired supporter state");
            }
            token = self.token.lock().await;
        }

        let settings = self.settings();
        if !settings.is_configured() {
            return Err(ApiError::Authentication);
        }
        let response = self
            .client
            .post(format!("{BASE_URL}/oauth/token"))
            .form(&[
                ("client_id", settings.client_id.as_str()),
                ("client_secret", settings.client_secret.as_str()),
                ("grant_type", "client_credentials"),
                ("scope", "public"),
            ])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status()));
        }

        let response: OAuthTokenResponse = response.json().await?;
        if response.access_token.is_empty() {
            return Err(ApiError::Authentication);
        }
        token.access_token = Some(response.access_token);
        token.expires_at = Some(token_expiry(response.expires_in));
        token.is_user = false;
        Ok(())
    }

    pub async fn search(
        &self,
        query: Option<&str>,
        mode: &str,
        status: &str,
        cursor: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<BeatmapSearchResponse, ApiError> {
        tokio::select! {
            _ = cancellation.cancelled() => return Err(ApiError::Cancelled),
            result = self.authenticate() => result?,
        }

        let url = search_url(query, mode, status, cursor)?;
        let mut response = self.send_search_request(&url, cancellation).await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let was_user = {
                let mut token = self.token.lock().await;
                let was_user = token.is_user;
                *token = TokenState::default();
                was_user
            };
            if was_user && let Err(error) = self.clear_stale_supporter() {
                tracing::warn!(%error, "could not persist revoked supporter state");
            }
            tokio::select! {
                _ = cancellation.cancelled() => return Err(ApiError::Cancelled),
                result = self.authenticate() => result?,
            }
            response = self.send_search_request(&url, cancellation).await?;
        }
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status()));
        }
        tokio::select! {
            _ = cancellation.cancelled() => Err(ApiError::Cancelled),
            result = response.json() => Ok(result?),
        }
    }

    pub async fn verify_saved_user(&self) -> Result<Option<OsuUser>, ApiError> {
        let is_user = {
            let token = self.token.lock().await;
            token.is_user && token.valid_token().is_some()
        };
        if !is_user {
            return Ok(None);
        }
        self.get_me().await.map(Some)
    }

    pub fn apply_verified_user(&self, user: &OsuUser) -> Result<AppSettings, ApiError> {
        let mut settings = self.settings();
        settings.username = user.username.clone();
        settings.is_logged_in = true;
        settings.is_supporter = user.is_supporter;
        settings.support_level = user.support_level;
        self.store_settings(settings.clone());
        storage::save_settings(&self.paths, &settings)?;
        Ok(settings)
    }

    pub fn clear_stale_supporter(&self) -> Result<AppSettings, ApiError> {
        let mut settings = self.settings();
        settings.username.clear();
        settings.is_logged_in = false;
        settings.is_supporter = false;
        settings.support_level = 0;
        settings.user_access_token = None;
        settings.user_token_expiry = None;
        self.store_settings(settings.clone());
        storage::save_settings(&self.paths, &settings)?;
        Ok(settings)
    }

    pub async fn login_user(&self) -> Result<OsuUser, ApiError> {
        let listener = TcpListener::bind(("127.0.0.1", 7270))
            .await
            .map_err(ApiError::OAuthListener)?;
        let state: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        let settings = self.settings();
        let mut authorization = Url::parse(&format!("{BASE_URL}/oauth/authorize"))?;
        authorization
            .query_pairs_mut()
            .append_pair("client_id", &settings.client_id)
            .append_pair("redirect_uri", REDIRECT_URI)
            .append_pair("response_type", "code")
            .append_pair("scope", "identify public")
            .append_pair("state", &state);
        open::that(authorization.as_str()).map_err(ApiError::Io)?;

        let code = wait_for_oauth_callback(&listener, &state).await?;
        let response = self
            .client
            .post(format!("{BASE_URL}/oauth/token"))
            .form(&[
                ("client_id", settings.client_id.as_str()),
                ("client_secret", settings.client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", REDIRECT_URI),
            ])
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status()));
        }
        let response: OAuthTokenResponse = response.json().await?;
        let expiry = token_expiry(response.expires_in);
        {
            let mut token = self.token.lock().await;
            token.access_token = Some(response.access_token.clone());
            token.expires_at = Some(expiry);
            token.is_user = true;
        }

        let mut settings = self.settings();
        settings.user_access_token = Some(response.access_token);
        settings.user_token_expiry = Some(expiry);
        self.store_settings(settings.clone());
        storage::save_settings(&self.paths, &settings)?;

        let user = self.get_me().await?;
        settings.username = user.username.clone();
        settings.is_logged_in = true;
        settings.is_supporter = user.is_supporter;
        settings.support_level = user.support_level;
        self.store_settings(settings.clone());
        storage::save_settings(&self.paths, &settings)?;
        Ok(user)
    }

    pub async fn logout(&self) -> Result<(), ApiError> {
        let mut settings = self.settings();
        settings.username.clear();
        settings.is_supporter = false;
        settings.support_level = 0;
        settings.is_logged_in = false;
        settings.user_access_token = None;
        settings.user_token_expiry = None;
        self.store_settings(settings.clone());
        *self.token.lock().await = TokenState::default();
        storage::save_settings(&self.paths, &settings)?;
        Ok(())
    }

    async fn get_me(&self) -> Result<OsuUser, ApiError> {
        let access_token = {
            let token = self.token.lock().await;
            if !token.is_user {
                return Err(ApiError::Authentication);
            }
            token
                .valid_token()
                .map(str::to_owned)
                .ok_or(ApiError::Authentication)?
        };
        let response = self
            .client
            .get(format!("{BASE_URL}/api/v2/me"))
            .bearer_auth(access_token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(ApiError::Http(response.status()));
        }
        Ok(response.json().await?)
    }

    async fn send_search_request(
        &self,
        url: &Url,
        cancellation: &CancellationToken,
    ) -> Result<reqwest::Response, ApiError> {
        let access_token = self
            .token
            .lock()
            .await
            .valid_token()
            .map(str::to_owned)
            .ok_or(ApiError::Authentication)?;
        let request = self
            .client
            .get(url.clone())
            .bearer_auth(access_token)
            .send();
        tokio::select! {
            _ = cancellation.cancelled() => Err(ApiError::Cancelled),
            response = request => Ok(response?),
        }
    }
}

async fn wait_for_oauth_callback(
    listener: &TcpListener,
    expected_state: &str,
) -> Result<String, ApiError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5 * 60);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(ApiError::OAuthTimeout);
        }
        let callback = tokio::time::timeout(remaining, listener.accept())
            .await
            .map_err(|_| ApiError::OAuthTimeout)?;
        let (mut stream, _) = callback?;
        let target = match read_callback_target(&mut stream, deadline).await {
            Ok(Some(target)) => target,
            Ok(None) | Err(ApiError::Io(_)) => {
                let _ = write_oauth_page(&mut stream, false).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        let callback_url = match Url::parse(&format!("http://localhost{target}")) {
            Ok(url) => url,
            Err(_) => {
                let _ = write_oauth_page(&mut stream, false).await;
                continue;
            }
        };
        let parameters = callback_url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        if callback_url.path() != "/callback"
            || parameters.get("state").map(String::as_str) != Some(expected_state)
        {
            let _ = write_oauth_page(&mut stream, false).await;
            continue;
        }
        if let Some(error) = parameters.get("error") {
            write_oauth_page(&mut stream, false).await?;
            return Err(ApiError::OAuthRejected(error.clone()));
        }
        if let Some(code) = parameters.get("code").filter(|code| !code.is_empty()) {
            write_oauth_page(&mut stream, true).await?;
            return Ok(code.clone());
        }
        let _ = write_oauth_page(&mut stream, false).await;
    }
}

async fn read_callback_target(
    stream: &mut tokio::net::TcpStream,
    deadline: tokio::time::Instant,
) -> Result<Option<String>, ApiError> {
    let mut request = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 2048];
    while request.len() < 16 * 1024 {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let read = match tokio::time::timeout(
            remaining.min(Duration::from_secs(5)),
            stream.read(&mut chunk),
        )
        .await
        {
            Ok(read) => read?,
            Err(_) => return Ok(None),
        };
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    Ok(request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .map(str::to_owned))
}

async fn write_oauth_page(
    stream: &mut tokio::net::TcpStream,
    accepted: bool,
) -> Result<(), ApiError> {
    let page = if accepted {
        "<html><body style='background:#1a1a2e;color:white;font-family:sans-serif;text-align:center;padding-top:20vh'><h1>Login successful</h1><p>You can close this tab.</p></body></html>"
    } else {
        "<html><body style='background:#1a1a2e;color:white;font-family:sans-serif;text-align:center;padding-top:20vh'><h1>Login cancelled</h1><p>You can close this tab.</p></body></html>"
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn token_expiry(expires_in: i64) -> DateTime<Utc> {
    Utc::now() + TimeDelta::seconds((expires_in - 60).max(0))
}

fn search_url(
    query: Option<&str>,
    mode: &str,
    status: &str,
    cursor: Option<&str>,
) -> Result<Url, url::ParseError> {
    let mut url = Url::parse(&format!("{BASE_URL}/api/v2/beatmapsets/search"))?;
    let sort = if matches!(status, "pending" | "graveyard") {
        "updated_desc"
    } else {
        "ranked_desc"
    };
    let mut parameters = url.query_pairs_mut();
    parameters.append_pair("sort", sort);
    if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
        parameters.append_pair("q", query);
    }
    if let Some(mode) = mode_index(mode) {
        parameters.append_pair("m", mode);
    }
    if !status.trim().is_empty() {
        parameters.append_pair("s", status);
    }
    if let Some(cursor) = cursor.filter(|cursor| !cursor.trim().is_empty()) {
        parameters.append_pair("cursor_string", cursor);
    }
    drop(parameters);
    Ok(url)
}

fn mode_index(mode: &str) -> Option<&'static str> {
    match mode {
        "osu" => Some("0"),
        "taiko" => Some("1"),
        "catch" => Some("2"),
        "mania" => Some("3"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ranked_search_url_with_legacy_parameters() {
        let url = search_url(Some("Camellia"), "mania", "qualified", Some("next page")).unwrap();
        let values = url.query_pairs().into_owned().collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                ("sort".to_owned(), "ranked_desc".to_owned()),
                ("q".to_owned(), "Camellia".to_owned()),
                ("m".to_owned(), "3".to_owned()),
                ("s".to_owned(), "qualified".to_owned()),
                ("cursor_string".to_owned(), "next page".to_owned()),
            ]
        );
    }

    #[test]
    fn omits_all_mode_and_sends_any_status() {
        let url = search_url(None, "all", "any", None).unwrap();
        assert_eq!(url.query(), Some("sort=ranked_desc&s=any"));
    }

    #[test]
    fn pending_uses_updated_sort() {
        let url = search_url(None, "osu", "pending", None).unwrap();
        assert!(url.query().unwrap().contains("sort=updated_desc"));
    }
}
