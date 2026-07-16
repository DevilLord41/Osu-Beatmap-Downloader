use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use eframe::egui::{
    self, Color32, FontFamily, FontId, Pos2, Rect, RichText, Sense, Stroke, TextStyle, Vec2,
};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::api::{ApiError, OsuApiClient};
use crate::audio::{self, AudioEvent, AudioPlayer, AudioService};
use crate::download::{
    self, DownloadEvent, DownloadRequest, DownloadService, DownloadSnapshot, DownloadStatus,
    QueueEntry, RateLimitSnapshot,
};
use crate::models::{AppSettings, BeatmapSearchResponse, BeatmapSet, OsuUser};
use crate::paths::DataPaths;
use crate::search::SearchQuery;
use crate::storage;

// Neutral surfaces keep the content dominant; osu! pink is the only navigation accent.
const APP_BG: Color32 = Color32::from_rgb(11, 11, 15);
const SURFACE: Color32 = Color32::from_rgb(18, 18, 24);
const SURFACE_RAISED: Color32 = Color32::from_rgb(24, 24, 32);
const SURFACE_HOVER: Color32 = Color32::from_rgb(30, 30, 40);
const BORDER: Color32 = Color32::from_rgb(42, 42, 53);
const BORDER_STRONG: Color32 = Color32::from_rgb(57, 57, 70);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(245, 245, 247);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(174, 174, 185);
const TEXT_MUTED: Color32 = Color32::from_rgb(125, 125, 139);
const ACCENT: Color32 = Color32::from_rgb(244, 93, 155);
const ACCENT_HOVER: Color32 = Color32::from_rgb(255, 112, 174);
const ACCENT_MUTED: Color32 = Color32::from_rgb(57, 29, 43);
const INFO: Color32 = Color32::from_rgb(118, 169, 250);
const SUCCESS: Color32 = Color32::from_rgb(100, 210, 166);
const WARNING: Color32 = Color32::from_rgb(232, 184, 92);
const DANGER: Color32 = Color32::from_rgb(248, 113, 113);
const RESULT_ROW_HEIGHT: f32 = 88.0;
const CONTROL_HEIGHT: f32 = 38.0;
const MODE_OSU_ICON: &[u8] = include_bytes!("../OsuBmDownloader/assets/mode-osu.png");
const MODE_TAIKO_ICON: &[u8] = include_bytes!("../OsuBmDownloader/assets/mode-taiko.png");
const MODE_CATCH_ICON: &[u8] = include_bytes!("../OsuBmDownloader/assets/mode-fruits.png");
const MODE_MANIA_ICON: &[u8] = include_bytes!("../OsuBmDownloader/assets/mode-mania.png");
const MAX_COVER_BYTES: usize = 10 * 1024 * 1024;
const MAX_CACHED_COVERS: usize = 256;

#[derive(Clone, Copy)]
enum Icon {
    Search,
    Refresh,
    Settings,
    Download,
    Close,
    Retry,
    Play,
    Pause,
    Queue,
    Check,
    Folder,
    User,
    Alert,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct FilterCache {
    #[serde(rename = "VisibleResults")]
    all_results: Vec<BeatmapSet>,
    #[serde(rename = "CursorString")]
    cursor_string: Option<String>,
    #[serde(rename = "HasMore")]
    has_more: bool,
}

impl Default for FilterCache {
    fn default() -> Self {
        Self {
            all_results: Vec::new(),
            cursor_string: None,
            has_more: true,
        }
    }
}

enum AppEvent {
    Authenticated {
        generation: u64,
        result: Result<UserVerification, String>,
    },
    SearchPage {
        generation: u64,
        result: Result<BeatmapSearchResponse, String>,
    },
    LoggedIn(Result<OsuUser, String>),
    LoggedOut(Result<(), String>),
    CoverLoaded {
        id: i32,
        url: String,
        result: Result<Arc<[u8]>, String>,
    },
}

enum UserVerification {
    NotApplicable,
    Verified(OsuUser),
    Failed(String),
}

struct CoverImage {
    url: String,
    bytes: Arc<[u8]>,
}

pub struct BeatmapApp {
    runtime: Option<Runtime>,
    paths: DataPaths,
    settings: AppSettings,
    settings_draft: AppSettings,
    api: OsuApiClient,
    cover_client: reqwest::Client,
    download_service: Arc<DownloadService>,
    audio_service: Arc<AudioService>,
    app_events_tx: mpsc::UnboundedSender<AppEvent>,
    app_events_rx: mpsc::UnboundedReceiver<AppEvent>,
    download_events_rx: mpsc::UnboundedReceiver<DownloadEvent>,
    audio_events_rx: mpsc::UnboundedReceiver<AudioEvent>,

    caches: HashMap<String, FilterCache>,
    beatmaps: Vec<BeatmapSet>,
    installed_ids: HashSet<i32>,
    queue: Vec<DownloadSnapshot>,
    queue_tokens: HashMap<i32, (u64, CancellationToken)>,
    pending_queue: Vec<QueueEntry>,
    next_download_attempt: u64,
    cover_images: HashMap<i32, CoverImage>,
    cover_order: VecDeque<i32>,
    cover_loading: HashMap<i32, String>,
    cover_failures: HashMap<i32, String>,

    selected_mode: String,
    selected_status: String,
    search_text: String,
    active_query: SearchQuery,
    active_cache_key: String,
    search_generation: u64,
    search_cancellation: CancellationToken,
    search_edited_at: Option<Instant>,
    loading: bool,
    search_paused: bool,
    loading_since: Option<Instant>,
    all_downloaded_hint: bool,
    authenticated: bool,
    authenticating: bool,
    authentication_generation: u64,

    no_video: bool,
    auto_install: bool,
    show_downloaded: bool,
    rate_limit: RateLimitSnapshot,

    audio_player: AudioPlayer,
    audio_request_id: Option<i32>,
    audio_cancellation: CancellationToken,

    show_settings: bool,
    first_run: bool,
    login_in_progress: bool,
    confirm_logout: bool,
    error: Option<String>,
    notice: Option<String>,
}

impl BeatmapApp {
    pub fn new(creation: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        egui_extras::install_image_loaders(&creation.egui_ctx);
        configure_style(&creation.egui_ctx);

        let paths = DataPaths::discover()?;
        let settings = storage::load_settings(&paths);
        let first_run = !settings.is_configured();
        let caches = storage::read_encrypted_json(&paths.search_cache_file)
            .ok()
            .flatten()
            .unwrap_or_default();
        let installed_ids = download::installed_ids(&paths, settings.osu_songs_path().as_deref());
        let pending_queue = download::load_queue(&paths);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("osu-bm-worker")
            .build()?;

        let (app_events_tx, app_events_rx) = mpsc::unbounded_channel();
        let (download_events_tx, download_events_rx) = mpsc::unbounded_channel();
        let (audio_events_tx, audio_events_rx) = mpsc::unbounded_channel();
        let api = OsuApiClient::new(settings.clone(), paths.clone())?;
        let cover_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!(
                "OsuBmDownloader/",
                env!("CARGO_PKG_VERSION"),
                "-rust"
            ))
            .build()?;
        let download_service = Arc::new(DownloadService::new(
            paths.clone(),
            &settings,
            download_events_tx,
        )?);
        let rate_limit = download_service.rate_limit();
        let audio_service = Arc::new(AudioService::new(paths.clone(), audio_events_tx)?);

        let mut app = Self {
            runtime: Some(runtime),
            paths,
            settings_draft: settings.clone(),
            settings,
            api,
            cover_client,
            download_service,
            audio_service,
            app_events_tx,
            app_events_rx,
            download_events_rx,
            audio_events_rx,
            caches,
            beatmaps: Vec::new(),
            installed_ids,
            queue: Vec::new(),
            queue_tokens: HashMap::new(),
            pending_queue,
            next_download_attempt: 1,
            cover_images: HashMap::new(),
            cover_order: VecDeque::new(),
            cover_loading: HashMap::new(),
            cover_failures: HashMap::new(),
            selected_mode: "all".to_owned(),
            selected_status: "ranked".to_owned(),
            search_text: String::new(),
            active_query: SearchQuery::default(),
            active_cache_key: "all|ranked|".to_owned(),
            search_generation: 0,
            search_cancellation: CancellationToken::new(),
            search_edited_at: None,
            loading: false,
            search_paused: false,
            loading_since: None,
            all_downloaded_hint: false,
            authenticated: false,
            authenticating: false,
            authentication_generation: 0,
            no_video: true,
            auto_install: true,
            show_downloaded: false,
            rate_limit,
            audio_player: AudioPlayer::default(),
            audio_request_id: None,
            audio_cancellation: CancellationToken::new(),
            show_settings: first_run,
            first_run,
            login_in_progress: false,
            confirm_logout: false,
            error: None,
            notice: None,
        };
        if !first_run {
            app.start_authentication(false);
        }
        Ok(app)
    }

    fn runtime(&self) -> &Runtime {
        self.runtime
            .as_ref()
            .expect("runtime is available while the application is running")
    }

    fn start_authentication(&mut self, replace_settings: bool) {
        self.authentication_generation = self.authentication_generation.wrapping_add(1);
        let generation = self.authentication_generation;
        self.authenticating = true;
        self.authenticated = false;
        let api = self.api.clone();
        let settings = self.settings.clone();
        let events = self.app_events_tx.clone();
        self.runtime().spawn(async move {
            if replace_settings {
                api.replace_settings(settings).await;
            }
            let result = match api.authenticate().await {
                Ok(()) => match api.verify_saved_user().await {
                    Ok(Some(user)) => Ok(UserVerification::Verified(user)),
                    Ok(None) => Ok(UserVerification::NotApplicable),
                    Err(ApiError::Http(reqwest::StatusCode::UNAUTHORIZED)) => {
                        Ok(UserVerification::NotApplicable)
                    }
                    Err(error) => Ok(UserVerification::Failed(error.to_string())),
                },
                Err(error) => Err(error.to_string()),
            };
            let _ = events.send(AppEvent::Authenticated { generation, result });
        });
    }

    fn start_login(&mut self) {
        if self.login_in_progress {
            return;
        }
        self.login_in_progress = true;
        let api = self.api.clone();
        let events = self.app_events_tx.clone();
        self.runtime().spawn(async move {
            let result = api.login_user().await.map_err(|error| error.to_string());
            let _ = events.send(AppEvent::LoggedIn(result));
        });
    }

    fn start_logout(&mut self) {
        let api = self.api.clone();
        let events = self.app_events_tx.clone();
        self.runtime().spawn(async move {
            let result = api.logout().await.map_err(|error| error.to_string());
            let _ = events.send(AppEvent::LoggedOut(result));
        });
    }

    fn reset_search(&mut self) {
        if !self.authenticated {
            return;
        }
        self.search_cancellation.cancel();
        self.search_cancellation = CancellationToken::new();
        self.search_generation = self.search_generation.wrapping_add(1);
        self.active_query = SearchQuery::parse(&self.search_text);
        self.active_cache_key = format!(
            "{}|{}|{}",
            self.selected_mode, self.selected_status, self.active_query.text
        );
        if self.selected_status == "qualified" {
            self.caches.remove(&self.active_cache_key);
        }
        self.caches
            .entry(self.active_cache_key.clone())
            .or_default();
        self.loading = false;
        self.search_paused = false;
        self.refresh_visible();
        let has_more = self
            .caches
            .get(&self.active_cache_key)
            .is_some_and(|cache| cache.has_more);
        if has_more {
            self.request_page();
        }
    }

    fn force_refresh(&mut self) {
        let query = SearchQuery::parse(&self.search_text);
        let key = format!(
            "{}|{}|{}",
            self.selected_mode, self.selected_status, query.text
        );
        self.caches.remove(&key);
        self.cover_failures.clear();
        self.reset_search();
    }

    fn request_cover(&mut self, id: i32, url: String, ctx: &egui::Context) {
        if self
            .cover_images
            .get(&id)
            .is_some_and(|image| image.url == url)
            || self
                .cover_loading
                .get(&id)
                .is_some_and(|loading_url| loading_url == &url)
            || self
                .cover_failures
                .get(&id)
                .is_some_and(|failed_url| failed_url == &url)
        {
            return;
        }
        if self.cover_images.remove(&id).is_some() {
            ctx.forget_image(&cover_uri(id));
        }
        self.cover_loading.insert(id, url.clone());
        let client = self.cover_client.clone();
        let events = self.app_events_tx.clone();
        let repaint = ctx.clone();
        self.runtime().spawn(async move {
            let result = async {
                let response = client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|error| error.to_string())?
                    .error_for_status()
                    .map_err(|error| error.to_string())?;
                if response
                    .content_length()
                    .is_some_and(|length| length > MAX_COVER_BYTES as u64)
                {
                    return Err("cover image exceeds the 10 MiB safety limit".to_owned());
                }
                let bytes = response.bytes().await.map_err(|error| error.to_string())?;
                if bytes.len() > MAX_COVER_BYTES {
                    return Err("cover image exceeds the 10 MiB safety limit".to_owned());
                }
                Ok(Arc::<[u8]>::from(bytes.to_vec()))
            }
            .await;
            let _ = events.send(AppEvent::CoverLoaded { id, url, result });
            repaint.request_repaint();
        });
    }

    fn request_page(&mut self) {
        if self.loading || self.search_paused || !self.authenticated {
            return;
        }
        let cache = self
            .caches
            .entry(self.active_cache_key.clone())
            .or_default();
        if !cache.has_more {
            return;
        }
        let cursor = cache.cursor_string.clone();
        let query = (!self.active_query.text.is_empty()).then(|| self.active_query.text.clone());
        let mode = self.selected_mode.clone();
        let status = self.selected_status.clone();
        let generation = self.search_generation;
        let cancellation = self.search_cancellation.clone();
        let api = self.api.clone();
        let events = self.app_events_tx.clone();
        self.loading = true;
        self.loading_since = Some(Instant::now());
        self.runtime().spawn(async move {
            let result = api
                .search(
                    query.as_deref(),
                    &mode,
                    &status,
                    cursor.as_deref(),
                    &cancellation,
                )
                .await
                .map_err(|error| error.to_string());
            let _ = events.send(AppEvent::SearchPage { generation, result });
        });
    }

    fn accept_search_page(&mut self, generation: u64, result: BeatmapSearchResponse) {
        if generation != self.search_generation {
            return;
        }
        let visible_before = self.beatmaps.len();
        let cache = self
            .caches
            .entry(self.active_cache_key.clone())
            .or_default();
        let previous_cursor = cache.cursor_string.clone();
        let previous_count = cache.all_results.len();
        let mut ids = cache
            .all_results
            .iter()
            .map(|set| set.id)
            .collect::<HashSet<_>>();
        for beatmap in result.beatmapsets {
            if ids.insert(beatmap.id) {
                cache.all_results.push(beatmap);
            }
        }
        cache.cursor_string = result
            .cursor_string
            .filter(|cursor| !cursor.trim().is_empty());
        cache.has_more = cache.cursor_string.is_some();
        let made_progress =
            cache.all_results.len() > previous_count || cache.cursor_string != previous_cursor;
        if cache.has_more && !made_progress {
            cache.has_more = false;
            self.search_paused = true;
            self.error = Some(
                "osu! returned a pagination cursor without new results. Use Refresh to retry."
                    .to_owned(),
            );
        }
        self.loading = false;
        self.loading_since = None;
        self.refresh_visible();
        if self.beatmaps.len() == visible_before
            && cache_has_more(&self.caches, &self.active_cache_key)
        {
            self.request_page();
        }
    }

    fn refresh_visible(&mut self) {
        let Some(cache) = self.caches.get(&self.active_cache_key) else {
            self.beatmaps.clear();
            return;
        };
        let queued = self
            .queue
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    DownloadStatus::Queued
                        | DownloadStatus::Downloading
                        | DownloadStatus::Extracting
                )
            })
            .map(|item| item.request.beatmap_set_id)
            .collect::<HashSet<_>>();
        self.beatmaps = cache
            .all_results
            .iter()
            .filter_map(|beatmap| {
                let mut beatmap = beatmap.clone();
                beatmap.is_downloaded = self.installed_ids.contains(&beatmap.id);
                beatmap.is_queued = queued.contains(&beatmap.id);
                if beatmap.is_downloaded && !self.show_downloaded {
                    return None;
                }
                self.active_query.matches(&beatmap).then_some(beatmap)
            })
            .collect();
        self.all_downloaded_hint =
            self.beatmaps.is_empty() && !cache.all_results.is_empty() && !cache.has_more;
    }

    fn enqueue_beatmap(&mut self, id: i32) {
        let Some(beatmap) = self.beatmaps.iter().find(|beatmap| beatmap.id == id) else {
            return;
        };
        let request = DownloadRequest {
            beatmap_set_id: beatmap.id,
            title: beatmap.title.clone(),
            artist: beatmap.artist.clone(),
            no_video: self.no_video,
            auto_install: self.auto_install,
        };
        self.enqueue_request(request, true);
    }

    fn enqueue_request(&mut self, request: DownloadRequest, show_limit_error: bool) -> bool {
        let replace_failed = if let Some(existing) = self
            .queue
            .iter()
            .find(|item| item.request.beatmap_set_id == request.beatmap_set_id)
        {
            if existing.status != DownloadStatus::Failed {
                return true;
            }
            true
        } else {
            false
        };
        let id = request.beatmap_set_id;
        let attempt_id = self.next_download_attempt;
        self.next_download_attempt = self.next_download_attempt.wrapping_add(1).max(1);
        let cancellation = CancellationToken::new();
        if let Err(limit) = self.download_service.spawn(
            self.runtime().handle(),
            attempt_id,
            request.clone(),
            cancellation.clone(),
        ) {
            if show_limit_error {
                self.error = Some(format!(
                    "Download limit reached ({}). Please wait or support osu! for unlimited downloads.",
                    limit.text()
                ));
            }
            return false;
        }
        if replace_failed {
            self.queue
                .retain(|item| item.request.beatmap_set_id != request.beatmap_set_id);
        }
        self.queue_tokens.insert(id, (attempt_id, cancellation));
        self.queue.push(DownloadSnapshot {
            attempt_id,
            request,
            status: DownloadStatus::Queued,
            progress: 0.0,
            error: None,
        });
        if let Some(beatmap) = self.beatmaps.iter_mut().find(|beatmap| beatmap.id == id) {
            beatmap.is_queued = true;
        }
        self.save_queue();
        true
    }

    fn retry_download(&mut self, id: i32) {
        let Some(item) = self
            .queue
            .iter()
            .find(|item| item.request.beatmap_set_id == id)
            .cloned()
        else {
            return;
        };
        let mut request = item.request;
        request.no_video = self.no_video;
        request.auto_install = self.auto_install;
        self.enqueue_request(request, true);
    }

    fn cancel_download(&mut self, id: i32) {
        if let Some((_, token)) = self.queue_tokens.remove(&id) {
            token.cancel();
        }
        self.queue.retain(|item| item.request.beatmap_set_id != id);
        if let Some(beatmap) = self.beatmaps.iter_mut().find(|beatmap| beatmap.id == id) {
            beatmap.is_queued = false;
        }
        self.save_queue();
    }

    fn cancel_all(&mut self) {
        for token in self.queue_tokens.drain().map(|(_, (_, token))| token) {
            token.cancel();
        }
        self.queue.clear();
        self.pending_queue.clear();
        for beatmap in &mut self.beatmaps {
            beatmap.is_queued = false;
        }
        self.save_queue();
    }

    fn download_all(&mut self) {
        if !self.settings.is_supporter {
            return;
        }
        let ids = self
            .beatmaps
            .iter()
            .filter(|beatmap| !beatmap.is_queued && !beatmap.is_downloaded)
            .map(|beatmap| beatmap.id)
            .take(100)
            .collect::<Vec<_>>();
        for id in ids {
            self.enqueue_beatmap(id);
        }
    }

    fn toggle_audio(&mut self, id: i32) {
        if !self.settings.is_supporter {
            return;
        }
        if self.audio_player.current_id() == Some(id) || self.audio_request_id == Some(id) {
            self.audio_cancellation.cancel();
            self.audio_cancellation = CancellationToken::new();
            self.audio_request_id = None;
            self.audio_player.stop();
            return;
        }
        self.audio_cancellation.cancel();
        self.audio_cancellation = CancellationToken::new();
        self.audio_player.stop();
        if self.installed_ids.contains(&id)
            && let Some(path) =
                audio::find_local_audio(self.settings.osu_songs_path().as_deref(), id)
        {
            if let Err(error) = self.audio_player.play(id, &path) {
                self.error = Some(format!("Could not play audio: {error}"));
            }
            return;
        }
        self.audio_request_id = Some(id);
        self.audio_service.request_preview(
            self.runtime().handle(),
            id,
            self.audio_cancellation.clone(),
        );
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.app_events_rx.try_recv() {
            match event {
                AppEvent::Authenticated { generation, result } => {
                    if generation != self.authentication_generation {
                        continue;
                    }
                    self.authenticating = false;
                    match result {
                        Ok(verification) => {
                            match verification {
                                UserVerification::Verified(user) => {
                                    match self.api.apply_verified_user(&user) {
                                        Ok(settings) => self.settings = settings,
                                        Err(error) => {
                                            self.error = Some(format!(
                                                "Could not save verified account: {error}"
                                            ));
                                        }
                                    }
                                }
                                UserVerification::NotApplicable if self.settings.is_supporter => {
                                    match self.api.clear_stale_supporter() {
                                        Ok(settings) => self.settings = settings,
                                        Err(error) => {
                                            self.error = Some(format!(
                                                "Could not update expired supporter status: {error}"
                                            ));
                                        }
                                    }
                                }
                                UserVerification::NotApplicable => {}
                                UserVerification::Failed(error) => {
                                    tracing::warn!(%error, "could not reverify supporter account");
                                }
                            }
                            self.settings_draft = self.settings.clone();
                            self.download_service.update_settings(&self.settings);
                            self.authenticated = true;
                            for entry in std::mem::take(&mut self.pending_queue) {
                                if !self.enqueue_request(entry.clone().into(), false) {
                                    self.pending_queue.push(entry);
                                }
                            }
                            self.save_queue();
                            self.reset_search();
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                AppEvent::SearchPage { generation, result } => match result {
                    Ok(result) => self.accept_search_page(generation, result),
                    Err(error) => {
                        if generation == self.search_generation {
                            self.loading = false;
                            self.loading_since = None;
                            if error != "request cancelled" {
                                self.search_paused = true;
                                self.error = Some(format!("Search failed: {error}"));
                            }
                        }
                    }
                },
                AppEvent::LoggedIn(result) => {
                    self.login_in_progress = false;
                    match result {
                        Ok(user) => {
                            self.settings = self.api.settings();
                            self.settings_draft = self.settings.clone();
                            self.download_service.update_settings(&self.settings);
                            self.notice = Some(if user.is_supporter {
                                format!(
                                    "Logged in as {}. Supporter features enabled.",
                                    user.username
                                )
                            } else {
                                format!(
                                    "Logged in as {}. This account is not an osu! supporter.",
                                    user.username
                                )
                            });
                        }
                        Err(error) => self.error = Some(format!("Login failed: {error}")),
                    }
                }
                AppEvent::LoggedOut(result) => match result {
                    Ok(()) => {
                        self.settings = self.api.settings();
                        self.settings_draft = self.settings.clone();
                        self.download_service.update_settings(&self.settings);
                    }
                    Err(error) => self.error = Some(format!("Logout failed: {error}")),
                },
                AppEvent::CoverLoaded { id, url, result } => {
                    if !self
                        .cover_loading
                        .get(&id)
                        .is_some_and(|loading_url| loading_url == &url)
                    {
                        continue;
                    }
                    self.cover_loading.remove(&id);
                    match result {
                        Ok(bytes) => {
                            self.cover_failures.remove(&id);
                            self.cover_order.retain(|cached_id| *cached_id != id);
                            while self.cover_images.len() >= MAX_CACHED_COVERS {
                                let Some(evicted) = self.cover_order.pop_front() else {
                                    break;
                                };
                                self.cover_images.remove(&evicted);
                                ctx.forget_image(&cover_uri(evicted));
                            }
                            self.cover_order.push_back(id);
                            self.cover_images.insert(id, CoverImage { url, bytes });
                        }
                        Err(error) => {
                            tracing::warn!(beatmap_set_id = id, %error, "cover image failed");
                            self.cover_failures.insert(id, url);
                        }
                    }
                }
            }
        }

        while let Ok(event) = self.download_events_rx.try_recv() {
            match event {
                DownloadEvent::Status {
                    id,
                    attempt_id,
                    status,
                } => {
                    if let Some(item) = self.queue.iter_mut().find(|item| {
                        item.request.beatmap_set_id == id && item.attempt_id == attempt_id
                    }) {
                        item.status = status;
                        self.save_queue();
                    }
                }
                DownloadEvent::Progress {
                    id,
                    attempt_id,
                    percent,
                } => {
                    if let Some(item) = self.queue.iter_mut().find(|item| {
                        item.request.beatmap_set_id == id && item.attempt_id == attempt_id
                    }) {
                        item.progress = percent;
                    }
                }
                DownloadEvent::Completed { id, attempt_id } => {
                    if !self.queue.iter().any(|item| {
                        item.request.beatmap_set_id == id && item.attempt_id == attempt_id
                    }) {
                        continue;
                    }
                    self.installed_ids.insert(id);
                    if self
                        .queue_tokens
                        .get(&id)
                        .is_some_and(|(attempt, _)| *attempt == attempt_id)
                    {
                        self.queue_tokens.remove(&id);
                    }
                    if self.audio_player.current_id() == Some(id) {
                        self.audio_player.stop();
                    }
                    self.audio_service.remove_cache(id);
                    self.refresh_visible();
                    self.save_queue();
                }
                DownloadEvent::Failed {
                    id,
                    attempt_id,
                    error,
                } => {
                    if self
                        .queue_tokens
                        .get(&id)
                        .is_some_and(|(attempt, _)| *attempt == attempt_id)
                    {
                        self.queue_tokens.remove(&id);
                    }
                    if let Some(item) = self.queue.iter_mut().find(|item| {
                        item.request.beatmap_set_id == id && item.attempt_id == attempt_id
                    }) {
                        item.status = DownloadStatus::Failed;
                        item.error = Some(error);
                        if let Some(beatmap) =
                            self.beatmaps.iter_mut().find(|beatmap| beatmap.id == id)
                        {
                            beatmap.is_queued = false;
                        }
                        self.save_queue();
                    }
                }
                DownloadEvent::Cancelled { id, attempt_id }
                | DownloadEvent::Removed { id, attempt_id } => {
                    let was_current = self.queue.iter().any(|item| {
                        item.request.beatmap_set_id == id && item.attempt_id == attempt_id
                    });
                    if !was_current {
                        continue;
                    }
                    if self
                        .queue_tokens
                        .get(&id)
                        .is_some_and(|(attempt, _)| *attempt == attempt_id)
                    {
                        self.queue_tokens.remove(&id);
                    }
                    self.queue.retain(|item| {
                        item.request.beatmap_set_id != id || item.attempt_id != attempt_id
                    });
                    if let Some(beatmap) = self.beatmaps.iter_mut().find(|beatmap| beatmap.id == id)
                    {
                        beatmap.is_queued = false;
                    }
                    self.save_queue();
                }
                DownloadEvent::RateLimit(snapshot) => self.rate_limit = snapshot,
            }
        }

        while let Ok(event) = self.audio_events_rx.try_recv() {
            match event {
                AudioEvent::Ready { id, path } if self.audio_request_id == Some(id) => {
                    self.audio_request_id = None;
                    if let Err(error) = self.audio_player.play(id, &path) {
                        self.error = Some(format!("Could not play preview: {error}"));
                    }
                }
                AudioEvent::Ready { .. } => {}
                AudioEvent::Failed { id, error } if self.audio_request_id == Some(id) => {
                    self.audio_request_id = None;
                    self.error = Some(format!("Could not load preview: {error}"));
                }
                AudioEvent::Failed { .. } => {}
            }
        }
    }

    fn save_queue(&self) {
        if let Err(error) = download::save_queue(&self.paths, &self.queue, &self.pending_queue) {
            tracing::warn!(%error, "could not save download queue");
        }
    }

    fn save_search_cache(&self) {
        let caches = self
            .caches
            .iter()
            .filter(|(_, cache)| cache.cursor_string.is_some() || !cache.all_results.is_empty())
            .map(|(key, cache)| (key.clone(), cache.clone()))
            .collect::<HashMap<_, _>>();
        if let Err(error) = storage::write_encrypted_json(&self.paths.search_cache_file, &caches) {
            tracing::warn!(%error, "could not save search cache");
        }
    }

    fn render_top_bar(&mut self, root: &mut egui::Ui) {
        let mut reset = false;
        let mut force_refresh = false;
        egui::Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin::symmetric(18, 12)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    draw_brand_mark(ui);
                    ui.add_space(2.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Beatmap Downloader")
                                .size(18.0)
                                .color(TEXT_PRIMARY)
                                .strong(),
                        );
                        ui.label(
                            RichText::new("Browse and install osu! beatmaps")
                                .size(12.0)
                                .color(TEXT_MUTED),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Settings, "Settings", CONTROL_HEIGHT).clicked()
                            && !self.authenticating
                            && !self.login_in_progress
                        {
                            self.settings_draft = self.settings.clone();
                            self.show_settings = true;
                        }

                        if self.settings.is_logged_in {
                            let account = secondary_button(
                                ui,
                                &self.settings.username,
                                Some(Icon::User),
                                !self.authenticating,
                            )
                            .on_hover_text("Account · click to log out");
                            if account.clicked() {
                                self.confirm_logout = true;
                            }
                        }

                        if self.settings.is_supporter {
                            badge(ui, "Supporter", ACCENT, ACCENT_MUTED);
                        } else if secondary_button(
                            ui,
                            if self.login_in_progress {
                                "Waiting for login"
                            } else {
                                "Supporter login"
                            },
                            Some(Icon::User),
                            !self.login_in_progress && !self.authenticating && !self.show_settings,
                        )
                        .clicked()
                        {
                            self.start_login();
                        }

                        if self.authenticating {
                            ui.spinner();
                            ui.label(RichText::new("Connecting").color(TEXT_SECONDARY));
                        }
                    });
                });

                ui.add_space(10.0);
                horizontal_rule(ui);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let search_width = (ui.available_width() - 48.0).max(240.0);
                    let response = ui
                        .allocate_ui_with_layout(
                            Vec2::new(search_width, CONTROL_HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| search_input(ui, &mut self.search_text),
                        )
                        .inner;
                    if response.changed() {
                        self.search_edited_at = Some(Instant::now());
                    }
                    if response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        self.search_edited_at = None;
                        reset = true;
                    }
                    if icon_button(ui, Icon::Refresh, "Refresh results", CONTROL_HEIGHT).clicked() {
                        force_refresh = true;
                    }
                });

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    section_label(ui, "MODE");
                    for (value, label, uri, bytes) in [
                        ("osu", "osu!", "bytes://mode/osu.png", MODE_OSU_ICON),
                        ("taiko", "taiko", "bytes://mode/taiko.png", MODE_TAIKO_ICON),
                        ("catch", "catch", "bytes://mode/catch.png", MODE_CATCH_ICON),
                        ("mania", "mania", "bytes://mode/mania.png", MODE_MANIA_ICON),
                    ] {
                        let selected = self.selected_mode == value;
                        if mode_button(ui, selected, uri, bytes, label).clicked() {
                            self.selected_mode = if selected {
                                "all".to_owned()
                            } else {
                                value.to_owned()
                            };
                            reset = true;
                        }
                    }

                    ui.add_space(4.0);
                    vertical_rule(ui, 24.0);
                    ui.add_space(4.0);
                    section_label(ui, "STATUS");
                    egui::ComboBox::from_id_salt("status")
                        .selected_text(status_label(&self.selected_status))
                        .width(112.0)
                        .show_ui(ui, |ui| {
                            for status in [
                                "ranked",
                                "qualified",
                                "loved",
                                "pending",
                                "graveyard",
                                "any",
                            ] {
                                if ui
                                    .selectable_value(
                                        &mut self.selected_status,
                                        status.to_owned(),
                                        status_label(status),
                                    )
                                    .changed()
                                {
                                    reset = true;
                                }
                            }
                        });

                    ui.add_space(4.0);
                    vertical_rule(ui, 24.0);
                    ui.add_space(4.0);
                    if toggle_chip(ui, self.no_video, "No video").clicked() {
                        self.no_video = !self.no_video;
                    }
                    if toggle_chip(ui, self.auto_install, "Auto install").clicked() {
                        self.auto_install = !self.auto_install;
                    }
                    if toggle_chip(ui, self.show_downloaded, "Show installed").clicked() {
                        self.show_downloaded = !self.show_downloaded;
                        reset = true;
                    }
                });
            });
        if force_refresh {
            self.force_refresh();
        } else if reset {
            self.reset_search();
        }
    }

    fn render_queue(&mut self, root: &mut egui::Ui) {
        let mut cancel = None;
        let mut retry = None;
        let mut cancel_all = false;
        egui::Panel::right("download_queue")
            .default_size(330.0)
            .min_size(290.0)
            .max_size(390.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    section_icon(ui, Icon::Queue);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Downloads")
                                .size(17.0)
                                .color(TEXT_PRIMARY)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} in this session",
                                self.queue.len() + self.pending_queue.len()
                            ))
                            .size(12.0)
                            .color(TEXT_MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if secondary_button(
                            ui,
                            "Cancel all",
                            None,
                            !self.queue.is_empty() || !self.pending_queue.is_empty(),
                        )
                        .clicked()
                        {
                            cancel_all = true;
                        }
                    });
                });

                ui.add_space(12.0);
                let (rate_color, rate_background) = if self.rate_limit.cooldown_seconds > 0 {
                    (WARNING, Color32::from_rgb(47, 37, 23))
                } else {
                    (SUCCESS, Color32::from_rgb(24, 43, 36))
                };
                info_strip(ui, &self.rate_limit.text(), rate_color, rate_background);
                if !self.pending_queue.is_empty() {
                    ui.add_space(8.0);
                    info_strip(
                        ui,
                        &format!(
                            "{} saved download(s) waiting for capacity",
                            self.pending_queue.len()
                        ),
                        WARNING,
                        Color32::from_rgb(47, 37, 23),
                    );
                }
                ui.add_space(12.0);
                horizontal_rule(ui);
                ui.add_space(8.0);

                if self.queue.is_empty() && self.pending_queue.is_empty() {
                    ui.add_space(44.0);
                    empty_state(
                        ui,
                        Icon::Queue,
                        "Queue is clear",
                        "Downloads and progress will appear here.",
                    );
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .show(ui, |ui| {
                        for item in &self.queue {
                            let id = item.request.beatmap_set_id;
                            let card = egui::Frame::new()
                                .fill(SURFACE_RAISED)
                                .stroke(Stroke::new(1.0, BORDER))
                                .corner_radius(10)
                                .inner_margin(egui::Margin::symmetric(10, 8))
                                .outer_margin(egui::Margin::symmetric(0, 3))
                                .show(ui, |ui| {
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    ui.horizontal(|ui| {
                                        let actions_width = if item.status == DownloadStatus::Failed
                                        {
                                            68.0
                                        } else {
                                            30.0
                                        };
                                        let title_width =
                                            (ui.available_width() - actions_width).max(80.0);
                                        ui.allocate_ui_with_layout(
                                            Vec2::new(title_width, 22.0),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(item.display_name())
                                                            .size(13.5)
                                                            .color(TEXT_PRIMARY)
                                                            .strong(),
                                                    )
                                                    .truncate(),
                                                )
                                                .on_hover_text(item.display_name());
                                            },
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if icon_button(
                                                    ui,
                                                    Icon::Close,
                                                    "Cancel download",
                                                    30.0,
                                                )
                                                .clicked()
                                                {
                                                    cancel = Some(id);
                                                }
                                                if item.status == DownloadStatus::Failed
                                                    && icon_button(
                                                        ui,
                                                        Icon::Retry,
                                                        "Retry download",
                                                        30.0,
                                                    )
                                                    .clicked()
                                                {
                                                    retry = Some(id);
                                                }
                                            },
                                        );
                                    });
                                    let status_color = download_status_color(item.status);
                                    ui.horizontal(|ui| {
                                        status_dot(ui, status_color);
                                        ui.label(
                                            RichText::new(item.status.text(item.progress))
                                                .size(12.0)
                                                .color(status_color),
                                        );
                                        if let Some(error) = &item.error {
                                            ui.label(
                                                RichText::new("/").size(11.5).color(TEXT_MUTED),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(error).size(11.5).color(DANGER),
                                                )
                                                .truncate(),
                                            )
                                            .on_hover_text(error);
                                        }
                                    });
                                });
                            if item.status == DownloadStatus::Downloading {
                                let track = Rect::from_min_size(
                                    Pos2::new(
                                        card.response.rect.left() + 10.0,
                                        card.response.rect.bottom() - 4.0,
                                    ),
                                    Vec2::new(card.response.rect.width() - 20.0, 2.0),
                                );
                                ui.painter().rect_filled(track, 1.0, BORDER_STRONG);
                                let progress = (item.progress / 100.0).clamp(0.0, 1.0);
                                if progress > 0.0 {
                                    ui.painter().rect_filled(
                                        Rect::from_min_size(
                                            track.min,
                                            Vec2::new(track.width() * progress, track.height()),
                                        ),
                                        1.0,
                                        ACCENT,
                                    );
                                }
                            }
                        }
                    });
            });
        if cancel_all {
            self.cancel_all();
        } else if let Some(id) = cancel {
            self.cancel_download(id);
        } else if let Some(id) = retry {
            self.retry_download(id);
        }
    }

    fn render_results(&mut self, root: &mut egui::Ui) {
        let mut download = None;
        let mut preview = None;
        let mut near_end = false;
        let mut download_all = false;
        let mut covers_to_load = Vec::new();
        let ctx = root.ctx().clone();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(APP_BG).inner_margin(16))
            .show(root, |ui| {
                let count = self.beatmaps.len();
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Beatmaps")
                                .size(22.0)
                                .color(TEXT_PRIMARY)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(format!(
                                "{} results  /  {}  /  {}",
                                count,
                                mode_label(&self.selected_mode),
                                status_label(&self.selected_status)
                            ))
                            .size(12.0)
                            .color(TEXT_MUTED),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.settings.is_supporter
                            && primary_button(
                                ui,
                                "Download all",
                                Some(Icon::Download),
                                !self.beatmaps.is_empty(),
                            )
                            .on_hover_text("Download up to 100 loaded beatmaps")
                            .clicked()
                        {
                            download_all = true;
                        }
                        if self.loading && count > 0 {
                            ui.spinner();
                            ui.label(RichText::new("Loading more").color(TEXT_SECONDARY));
                        }
                    });
                });
                ui.add_space(12.0);
                horizontal_rule(ui);
                ui.add_space(10.0);

                if self.loading && count == 0 {
                    let slow = self
                        .loading_since
                        .is_some_and(|started| started.elapsed() >= Duration::from_secs(3));
                    loading_skeletons(
                        ui,
                        if slow {
                            "Searching broadly because filters hide many maps"
                        } else {
                            "Finding beatmaps"
                        },
                    );
                } else if self.all_downloaded_hint {
                    ui.add_space(72.0);
                    empty_state(
                        ui,
                        Icon::Check,
                        "Everything is installed",
                        "Enable Show installed or adjust the current filters.",
                    );
                } else if self.beatmaps.is_empty() && self.authenticated {
                    ui.add_space(72.0);
                    empty_state(
                        ui,
                        Icon::Search,
                        "No beatmaps found",
                        "Try a broader search, another status, or fewer filters.",
                    );
                } else if count > 0 {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show_rows(ui, RESULT_ROW_HEIGHT, count, |ui, rows| {
                            near_end = rows.end.saturating_add(4) >= count;
                            for index in rows {
                                let beatmap = &self.beatmaps[index];
                                let id = beatmap.id;
                                egui::Frame::new()
                                    .fill(SURFACE)
                                    .stroke(Stroke::new(1.0, BORDER))
                                    .corner_radius(12)
                                    .inner_margin(egui::Margin::symmetric(10, 10))
                                    .show(ui, |ui| {
                                        ui.set_min_height(68.0);
                                        ui.set_min_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            let cover_size = Vec2::new(106.0, 68.0);
                                            if let Some(url) = beatmap.cover_url() {
                                                if let Some(cover) = self
                                                    .cover_images
                                                    .get(&id)
                                                    .filter(|cover| cover.url == url)
                                                {
                                                    let image = egui::Image::from_bytes(
                                                        cover_uri(id),
                                                        Arc::clone(&cover.bytes),
                                                    )
                                                    .fit_to_exact_size(cover_size)
                                                    .corner_radius(8)
                                                    .alt_text(format!(
                                                        "Cover for {} by {}",
                                                        beatmap.title, beatmap.artist
                                                    ))
                                                    .sense(Sense::click());
                                                    let response = ui
                                                        .allocate_ui_with_layout(
                                                            cover_size,
                                                            egui::Layout::centered_and_justified(
                                                                egui::Direction::TopDown,
                                                            ),
                                                            |ui| {
                                                                ui.set_min_size(cover_size);
                                                                ui.add_sized(cover_size, image)
                                                            },
                                                        )
                                                        .inner
                                                        .on_hover_text(
                                                            if self.settings.is_supporter {
                                                                "Play or pause preview"
                                                            } else {
                                                                "Audio previews require osu! supporter"
                                                            },
                                                        );
                                                    let active_preview =
                                                        self.audio_player.current_id() == Some(id)
                                                            || self.audio_request_id == Some(id);
                                                    if response.hovered() || active_preview {
                                                        ui.painter().circle_filled(
                                                            response.rect.center(),
                                                            17.0,
                                                            Color32::from_black_alpha(180),
                                                        );
                                                        paint_icon(
                                                            ui.painter(),
                                                            if active_preview {
                                                                Icon::Pause
                                                            } else {
                                                                Icon::Play
                                                            },
                                                            Rect::from_center_size(
                                                                response.rect.center(),
                                                                Vec2::splat(18.0),
                                                            ),
                                                            TEXT_PRIMARY,
                                                            1.8,
                                                        );
                                                    }
                                                    if response.clicked() {
                                                        preview = Some(id);
                                                    }
                                                } else {
                                                    let loading =
                                                        self.cover_loading.contains_key(&id);
                                                    let failed =
                                                        self.cover_failures.get(&id).is_some_and(
                                                            |failed_url| failed_url == url,
                                                        );
                                                    if !loading && !failed {
                                                        covers_to_load.push((id, url.to_owned()));
                                                    }
                                                    cover_placeholder(ui, cover_size, loading);
                                                }
                                            } else {
                                                cover_placeholder(ui, cover_size, false);
                                            }

                                            let info_width =
                                                (ui.available_width() - 56.0).max(120.0);
                                            ui.allocate_ui_with_layout(
                                                Vec2::new(info_width, 68.0),
                                                egui::Layout::top_down(egui::Align::Min),
                                                |ui| {
                                                    ui.set_min_size(Vec2::new(info_width, 68.0));
                                                    let title_response = ui
                                                        .add(
                                                            egui::Label::new(
                                                                RichText::new(&beatmap.title)
                                                                    .size(15.0)
                                                                    .color(TEXT_PRIMARY)
                                                                    .strong(),
                                                            )
                                                            .truncate()
                                                            .sense(Sense::click()),
                                                        )
                                                        .on_hover_text(format!(
                                                            "{} by {}",
                                                            beatmap.title, beatmap.artist
                                                        ));
                                                    if title_response.clicked() {
                                                        preview = Some(id);
                                                    }
                                                    let attribution = format!(
                                                        "{}  /  mapped by {}",
                                                        beatmap.artist, beatmap.creator
                                                    );
                                                    ui.add(
                                                        egui::Label::new(
                                                            RichText::new(&attribution)
                                                                .size(12.0)
                                                                .color(TEXT_SECONDARY),
                                                        )
                                                        .truncate(),
                                                    )
                                                    .on_hover_text(attribution);
                                                    ui.add_space(5.0);
                                                    ui.horizontal(|ui| {
                                                        let color = status_color(&beatmap.status);
                                                        badge(
                                                            ui,
                                                            status_label(&beatmap.status),
                                                            color,
                                                            status_background(&beatmap.status),
                                                        );
                                                        let date = beatmap.date_text();
                                                        if !date.is_empty() {
                                                            meta_label(ui, &date);
                                                        }
                                                        let stars = beatmap.star_range_text();
                                                        let stars = stars
                                                            .strip_prefix("\u{2605} ")
                                                            .unwrap_or(&stars);
                                                        meta_label(ui, &format!("{stars} SR"));
                                                    });
                                                },
                                            );

                                            ui.allocate_ui_with_layout(
                                                Vec2::new(48.0, 68.0),
                                                egui::Layout::top_down(egui::Align::Center),
                                                |ui| {
                                                    ui.set_min_size(Vec2::new(48.0, 68.0));
                                                    ui.add_space(14.0);
                                                    if beatmap.is_downloaded {
                                                        state_icon(
                                                            ui,
                                                            Icon::Check,
                                                            SUCCESS,
                                                            "Installed",
                                                        );
                                                    } else if beatmap.is_queued {
                                                        state_icon(ui, Icon::Queue, INFO, "Queued");
                                                    } else if accent_icon_button(
                                                        ui,
                                                        Icon::Download,
                                                        &format!("Download {}", beatmap.title),
                                                        40.0,
                                                    )
                                                    .clicked()
                                                    {
                                                        download = Some(id);
                                                    }
                                                },
                                            );
                                        });
                                    });
                            }
                        });
                }
            });
        if let Some(id) = download {
            self.enqueue_beatmap(id);
        }
        if let Some(id) = preview {
            self.toggle_audio(id);
        }
        if download_all {
            self.download_all();
        }
        for (id, url) in covers_to_load {
            self.request_cover(id, url, &ctx);
        }
        if near_end && !self.loading {
            self.request_page();
        }
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut save = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("settings_modal"))
            .backdrop_color(Color32::from_black_alpha(185))
            .frame(modal_frame())
            .show(ctx, |ui| {
                ui.set_width(560.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(if self.first_run {
                                "Set up Beatmap Downloader"
                            } else {
                                "Settings"
                            })
                            .size(21.0)
                            .color(TEXT_PRIMARY)
                            .strong(),
                        );
                        ui.label(
                            RichText::new(
                                "Connect osu! API v2 and choose where beatmaps are installed.",
                            )
                            .size(12.5)
                            .color(TEXT_SECONDARY),
                        );
                    });
                    if !self.first_run {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, Icon::Close, "Close settings", 36.0).clicked() {
                                cancel = true;
                            }
                        });
                    }
                });

                ui.add_space(18.0);
                horizontal_rule(ui);
                ui.add_space(16.0);
                section_label(ui, "INSTALLATION");
                ui.add_space(6.0);
                field_label(ui, "osu! folder");
                ui.horizontal(|ui| {
                    let field_width = (ui.available_width() - 102.0).max(240.0);
                    text_field(ui, &mut self.settings_draft.osu_path, field_width, false);
                    if secondary_button(ui, "Browse", Some(Icon::Folder), true).clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .set_title("Select osu! installation folder")
                            .pick_folder()
                    {
                        self.settings_draft.osu_path = path.display().to_string();
                    }
                });

                ui.add_space(18.0);
                section_label(ui, "API CREDENTIALS");
                ui.add_space(6.0);
                if ui
                    .link("Create credentials on your osu! account page")
                    .clicked()
                {
                    let _ = open::that("https://osu.ppy.sh/home/account/edit");
                }
                ui.label(
                    RichText::new("Callback URL: http://localhost:7270/callback")
                        .size(11.5)
                        .color(TEXT_MUTED),
                );
                ui.add_space(10.0);
                field_label(ui, "Client ID");
                let field_width = ui.available_width();
                text_field(ui, &mut self.settings_draft.client_id, field_width, false);
                ui.add_space(10.0);
                field_label(ui, "Client secret");
                let field_width = ui.available_width();
                text_field(
                    ui,
                    &mut self.settings_draft.client_secret,
                    field_width,
                    true,
                );

                ui.add_space(16.0);
                info_strip(
                    ui,
                    "Free accounts can download 30 beatmap sets per rolling hour.",
                    INFO,
                    Color32::from_rgb(23, 35, 52),
                );
                ui.add_space(18.0);
                horizontal_rule(ui);
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, "Save settings", None, true).clicked() {
                        save = true;
                    }
                    if !self.first_run && secondary_button(ui, "Cancel", None, true).clicked() {
                        cancel = true;
                    }
                });
            });
        if !self.first_run && modal.should_close() {
            cancel = true;
        }
        if save {
            self.settings_draft.client_id = self.settings_draft.client_id.trim().to_owned();
            self.settings_draft.client_secret = self.settings_draft.client_secret.trim().to_owned();
            self.settings_draft.osu_path = self.settings_draft.osu_path.trim().to_owned();
            if !self.settings_draft.is_configured() {
                self.error = Some("Client ID and Client Secret are required.".to_owned());
            } else if let Err(error) = storage::save_settings(&self.paths, &self.settings_draft) {
                self.error = Some(format!("Could not save settings: {error}"));
            } else {
                self.settings = self.settings_draft.clone();
                self.download_service.update_settings(&self.settings);
                self.installed_ids =
                    download::installed_ids(&self.paths, self.settings.osu_songs_path().as_deref());
                self.show_settings = false;
                self.first_run = false;
                self.start_authentication(true);
            }
        } else if cancel {
            self.show_settings = false;
        }
    }

    fn render_dialogs(&mut self, ctx: &egui::Context) {
        if self.confirm_logout {
            let mut confirm = false;
            let mut cancel = false;
            let modal = egui::Modal::new(egui::Id::new("logout_modal"))
                .backdrop_color(Color32::from_black_alpha(185))
                .frame(modal_frame())
                .show(ctx, |ui| {
                    ui.set_width(400.0);
                    modal_heading(
                        ui,
                        Icon::User,
                        ACCENT,
                        "Log out?",
                        &format!(
                            "You will need to reconnect {} to restore supporter features.",
                            self.settings.username
                        ),
                    );
                    ui.add_space(18.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if danger_button(ui, "Log out", true).clicked() {
                            confirm = true;
                        }
                        if secondary_button(ui, "Cancel", None, true).clicked() {
                            cancel = true;
                        }
                    });
                });
            if confirm {
                self.confirm_logout = false;
                self.start_logout();
            } else if cancel || modal.should_close() {
                self.confirm_logout = false;
            }
        }

        if let Some(message) = self.error.clone() {
            let mut close = false;
            let modal = egui::Modal::new(egui::Id::new("error_modal"))
                .backdrop_color(Color32::from_black_alpha(185))
                .frame(modal_frame())
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    modal_heading(ui, Icon::Alert, DANGER, "Something went wrong", &message);
                    ui.add_space(18.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Close", None, true).clicked() {
                            close = true;
                        }
                    });
                });
            if close || modal.should_close() {
                self.error = None;
            }
        }
        if let Some(message) = self.notice.clone() {
            let mut close = false;
            let modal = egui::Modal::new(egui::Id::new("notice_modal"))
                .backdrop_color(Color32::from_black_alpha(185))
                .frame(modal_frame())
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    modal_heading(ui, Icon::Check, SUCCESS, "Account updated", &message);
                    ui.add_space(18.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if primary_button(ui, "Close", None, true).clicked() {
                            close = true;
                        }
                    });
                });
            if close || modal.should_close() {
                self.notice = None;
            }
        }
    }
}

impl eframe::App for BeatmapApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        if self.settings.is_supporter
            && self
                .settings
                .user_token_expiry
                .is_none_or(|expiry| Utc::now() >= expiry)
            && let Err(error) = self.api.clear_stale_supporter()
        {
            self.error = Some(format!("Could not save expired account state: {error}"));
        }
        let api_settings = self.api.settings();
        if api_settings.is_supporter != self.settings.is_supporter
            || api_settings.is_logged_in != self.settings.is_logged_in
            || api_settings.user_access_token != self.settings.user_access_token
            || api_settings.username != self.settings.username
            || api_settings.support_level != self.settings.support_level
        {
            self.settings = api_settings;
            self.settings_draft = self.settings.clone();
            self.download_service.update_settings(&self.settings);
        }
        self.rate_limit = self.download_service.rate_limit();
        self.audio_player.reconcile();
        while self.authenticated
            && !self.pending_queue.is_empty()
            && (self.rate_limit.unlimited || self.rate_limit.remaining > 0)
        {
            let entry = self.pending_queue.remove(0);
            if !self.enqueue_request(entry.clone().into(), false) {
                self.pending_queue.insert(0, entry);
                break;
            }
            self.rate_limit = self.download_service.rate_limit();
        }
        if self
            .search_edited_at
            .is_some_and(|changed| changed.elapsed() >= Duration::from_millis(400))
        {
            self.search_edited_at = None;
            self.reset_search();
        }

        let has_active_downloads = self.queue.iter().any(|item| {
            matches!(
                item.status,
                DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Extracting
            )
        });
        let active = self.loading
            || self.authenticating
            || self.login_in_progress
            || has_active_downloads
            || self.audio_request_id.is_some()
            || self.search_edited_at.is_some();
        ctx.request_repaint_after(if active {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        });
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.render_top_bar(ui);
        self.render_queue(ui);
        self.render_results(ui);
        self.render_settings(&ctx);
        self.render_dialogs(&ctx);
    }
}

impl Drop for BeatmapApp {
    fn drop(&mut self) {
        self.search_cancellation.cancel();
        self.audio_cancellation.cancel();
        for (_, token) in self.queue_tokens.values() {
            token.cancel();
        }
        self.audio_player.stop();
        self.save_queue();
        self.save_search_cache();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

fn cache_has_more(caches: &HashMap<String, FilterCache>, key: &str) -> bool {
    caches.get(key).is_some_and(|cache| cache.has_more)
}

fn cover_uri(id: i32) -> String {
    format!("bytes://cover/{id}")
}

fn draw_brand_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::hover());
    ui.painter().rect_filled(rect, 11.0, ACCENT);
    paint_icon(
        ui.painter(),
        Icon::Download,
        rect.shrink(9.0),
        Color32::WHITE,
        2.0,
    );
}

fn section_icon(ui: &mut egui::Ui, icon: Icon) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, ACCENT_MUTED);
    paint_icon(ui.painter(), icon, rect.shrink(9.0), ACCENT_HOVER, 1.8);
}

fn horizontal_rule(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [rect.left_center(), rect.right_center()],
        Stroke::new(1.0, BORDER),
    );
}

fn vertical_rule(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(1.0, height), Sense::hover());
    ui.painter().line_segment(
        [rect.center_top(), rect.center_bottom()],
        Stroke::new(1.0, BORDER),
    );
}

fn section_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(10.5).color(TEXT_MUTED).strong());
}

fn field_label(ui: &mut egui::Ui, label: &str) {
    ui.label(
        RichText::new(label)
            .size(12.0)
            .color(TEXT_SECONDARY)
            .strong(),
    );
    ui.add_space(4.0);
}

fn badge(ui: &mut egui::Ui, text: &str, foreground: Color32, background: Color32) {
    egui::Frame::new()
        .fill(background)
        .stroke(Stroke::new(1.0, foreground.gamma_multiply(0.35)))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(10.5).color(foreground).strong());
        });
}

fn meta_label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(TEXT_MUTED));
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

fn info_strip(ui: &mut egui::Ui, text: &str, foreground: Color32, background: Color32) {
    egui::Frame::new()
        .fill(background)
        .stroke(Stroke::new(1.0, foreground.gamma_multiply(0.25)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                status_dot(ui, foreground);
                ui.add(egui::Label::new(RichText::new(text).size(11.5).color(foreground)).wrap());
            });
        });
}

fn empty_state(ui: &mut egui::Ui, icon: Icon, title: &str, body: &str) {
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(48.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 24.0, SURFACE_RAISED);
        ui.painter()
            .circle_stroke(rect.center(), 24.0, Stroke::new(1.0, BORDER));
        paint_icon(ui.painter(), icon, rect.shrink(14.0), TEXT_SECONDARY, 1.8);
        ui.add_space(12.0);
        ui.label(RichText::new(title).size(15.0).color(TEXT_PRIMARY).strong());
        ui.add_space(3.0);
        ui.label(RichText::new(body).size(12.0).color(TEXT_MUTED));
    });
}

fn loading_skeletons(ui: &mut egui::Ui, message: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new(message).color(TEXT_SECONDARY));
    });
    ui.add_space(10.0);
    for _ in 0..5 {
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), RESULT_ROW_HEIGHT),
            Sense::hover(),
        );
        ui.painter().rect_filled(rect, 12.0, SURFACE);
        ui.painter().rect_stroke(
            rect,
            12.0,
            Stroke::new(1.0, BORDER),
            egui::StrokeKind::Inside,
        );
        let cover = Rect::from_min_size(rect.min + Vec2::new(10.0, 10.0), Vec2::new(106.0, 68.0));
        ui.painter().rect_filled(cover, 8.0, SURFACE_RAISED);
        let title = Rect::from_min_size(
            Pos2::new(cover.right() + 14.0, rect.top() + 15.0),
            Vec2::new((rect.width() * 0.38).max(120.0), 10.0),
        );
        let subtitle = Rect::from_min_size(
            Pos2::new(cover.right() + 14.0, rect.top() + 36.0),
            Vec2::new((rect.width() * 0.25).max(90.0), 8.0),
        );
        ui.painter().rect_filled(title, 4.0, BORDER_STRONG);
        ui.painter().rect_filled(subtitle, 4.0, BORDER);
    }
}

fn cover_placeholder(ui: &mut egui::Ui, size: Vec2, loading: bool) {
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.set_min_size(size);
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 8.0, SURFACE_RAISED);
            ui.painter().rect_stroke(
                rect,
                8.0,
                Stroke::new(1.0, BORDER),
                egui::StrokeKind::Inside,
            );
            if loading {
                ui.spinner();
            } else {
                paint_icon(
                    ui.painter(),
                    Icon::Image,
                    Rect::from_center_size(rect.center(), Vec2::splat(20.0)),
                    TEXT_MUTED,
                    1.6,
                );
            }
        },
    );
}

fn search_input(ui: &mut egui::Ui, value: &mut String) -> egui::Response {
    let output = egui::Frame::new()
        .fill(SURFACE_RAISED)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(10, 0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                paint_icon(ui.painter(), Icon::Search, icon_rect, TEXT_MUTED, 1.6);
                ui.add_sized(
                    [ui.available_width(), CONTROL_HEIGHT],
                    egui::TextEdit::singleline(value)
                        .hint_text("Search beatmaps or try star>=5, bpm>=180")
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::symmetric(2, 8)),
                )
            })
            .inner
        });
    if output.inner.has_focus() {
        ui.painter().rect_stroke(
            output.response.rect,
            10.0,
            Stroke::new(1.5, ACCENT),
            egui::StrokeKind::Inside,
        );
    }
    output.inner
}

fn text_field(ui: &mut egui::Ui, value: &mut String, width: f32, password: bool) -> egui::Response {
    ui.add_sized(
        [width, 40.0],
        egui::TextEdit::singleline(value)
            .password(password)
            .margin(egui::Margin::symmetric(11, 9))
            .background_color(SURFACE_RAISED),
    )
}

fn mode_button(
    ui: &mut egui::Ui,
    selected: bool,
    uri: &'static str,
    bytes: &'static [u8],
    label: &str,
) -> egui::Response {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        widgets.inactive.weak_bg_fill = if selected {
            ACCENT_MUTED
        } else {
            SURFACE_RAISED
        };
        widgets.inactive.bg_stroke = Stroke::new(
            1.0,
            if selected {
                ACCENT.gamma_multiply(0.65)
            } else {
                BORDER
            },
        );
        widgets.hovered.weak_bg_fill = if selected {
            ACCENT_MUTED
        } else {
            SURFACE_HOVER
        };
        widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.7));
        let image = egui::Image::from_bytes(uri, bytes)
            .fit_to_exact_size(Vec2::splat(21.0))
            .alt_text(label)
            .tint(if selected {
                TEXT_PRIMARY
            } else {
                TEXT_SECONDARY
            });
        ui.add_sized(
            [40.0, CONTROL_HEIGHT],
            egui::Button::new(image).corner_radius(9),
        )
        .on_hover_text(if selected {
            format!("{label} selected · click again for all modes")
        } else {
            format!("Filter by {label}")
        })
    })
    .inner
}

fn toggle_chip(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        if selected {
            widgets.inactive.weak_bg_fill = ACCENT_MUTED;
            widgets.inactive.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.6));
            widgets.inactive.fg_stroke = Stroke::new(1.0, ACCENT_HOVER);
        }
        widgets.hovered.weak_bg_fill = if selected {
            ACCENT_MUTED
        } else {
            SURFACE_HOVER
        };
        widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.65));
        let response = ui.add_sized(
            [0.0, 34.0],
            egui::Button::new(RichText::new(format!("     {label}")).size(12.0)).corner_radius(8),
        );
        let center = Pos2::new(response.rect.left() + 15.0, response.rect.center().y);
        ui.painter().circle_stroke(
            center,
            6.0,
            Stroke::new(1.3, if selected { ACCENT_HOVER } else { TEXT_MUTED }),
        );
        if selected {
            paint_icon(
                ui.painter(),
                Icon::Check,
                Rect::from_center_size(center, Vec2::splat(9.0)),
                ACCENT_HOVER,
                1.4,
            );
        }
        response
    })
    .inner
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, accessible_label: &str, size: f32) -> egui::Response {
    let response = ui.add_sized(
        [size, size],
        egui::Button::new(
            RichText::new(accessible_label)
                .size(1.0)
                .color(Color32::TRANSPARENT),
        )
        .corner_radius(9),
    );
    let mut color = ui.style().interact(&response).fg_stroke.color;
    if !response.enabled() {
        color = color.gamma_multiply(0.45);
    }
    paint_icon(
        ui.painter(),
        icon,
        Rect::from_center_size(response.rect.center(), Vec2::splat(size * 0.46)),
        color,
        1.7,
    );
    response.on_hover_text(accessible_label)
}

fn accent_icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    accessible_label: &str,
    size: f32,
) -> egui::Response {
    ui.scope(|ui| {
        set_button_colors(ui, ACCENT, ACCENT_HOVER, Color32::WHITE, ACCENT);
        icon_button(ui, icon, accessible_label, size)
    })
    .inner
}

fn primary_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Option<Icon>,
    enabled: bool,
) -> egui::Response {
    ui.scope(|ui| {
        set_button_colors(ui, ACCENT, ACCENT_HOVER, Color32::WHITE, ACCENT);
        text_icon_button(ui, label, icon, enabled)
    })
    .inner
}

fn secondary_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Option<Icon>,
    enabled: bool,
) -> egui::Response {
    text_icon_button(ui, label, icon, enabled)
}

fn danger_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        set_button_colors(
            ui,
            Color32::from_rgb(185, 55, 65),
            DANGER,
            Color32::WHITE,
            DANGER,
        );
        text_icon_button(ui, label, None, enabled)
    })
    .inner
}

fn text_icon_button(
    ui: &mut egui::Ui,
    label: &str,
    icon: Option<Icon>,
    enabled: bool,
) -> egui::Response {
    let text = if icon.is_some() {
        format!("    {label}")
    } else {
        label.to_owned()
    };
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(text).size(12.5).strong())
            .min_size(Vec2::new(0.0, CONTROL_HEIGHT))
            .corner_radius(9),
    );
    if let Some(icon) = icon {
        let mut color = ui.style().interact(&response).fg_stroke.color;
        if !enabled {
            color = color.gamma_multiply(0.45);
        }
        let center = Pos2::new(response.rect.left() + 16.0, response.rect.center().y);
        paint_icon(
            ui.painter(),
            icon,
            Rect::from_center_size(center, Vec2::splat(16.0)),
            color,
            1.6,
        );
    }
    response
}

fn set_button_colors(
    ui: &mut egui::Ui,
    inactive: Color32,
    hovered: Color32,
    foreground: Color32,
    border: Color32,
) {
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.bg_fill = inactive;
    widgets.inactive.weak_bg_fill = inactive;
    widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    widgets.inactive.fg_stroke = Stroke::new(1.0, foreground);
    widgets.hovered.bg_fill = hovered;
    widgets.hovered.weak_bg_fill = hovered;
    widgets.hovered.bg_stroke = Stroke::new(1.0, hovered);
    widgets.hovered.fg_stroke = Stroke::new(1.0, foreground);
    widgets.active.bg_fill = hovered;
    widgets.active.weak_bg_fill = hovered;
    widgets.active.bg_stroke = Stroke::new(1.0, foreground.gamma_multiply(0.7));
    widgets.active.fg_stroke = Stroke::new(1.0, foreground);
}

fn state_icon(ui: &mut egui::Ui, icon: Icon, color: Color32, label: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 18.0, color.gamma_multiply(0.16));
    ui.painter().circle_stroke(
        rect.center(),
        18.0,
        Stroke::new(1.0, color.gamma_multiply(0.45)),
    );
    paint_icon(ui.painter(), icon, rect.shrink(12.0), color, 1.8);
    response.on_hover_text(label);
}

fn modal_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .corner_radius(16)
        .inner_margin(egui::Margin::same(24))
        .shadow(egui::epaint::Shadow {
            offset: [0, 12],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(150),
        })
}

fn modal_heading(ui: &mut egui::Ui, icon: Icon, color: Color32, title: &str, body: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(42.0), Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 21.0, color.gamma_multiply(0.16));
        paint_icon(ui.painter(), icon, rect.shrink(12.0), color, 1.8);
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).size(19.0).color(TEXT_PRIMARY).strong());
            ui.add(egui::Label::new(RichText::new(body).size(12.5).color(TEXT_SECONDARY)).wrap());
        });
    });
}

fn paint_icon(painter: &egui::Painter, icon: Icon, rect: Rect, color: Color32, width: f32) {
    let center = rect.center();
    let size = rect.width().min(rect.height());
    let stroke = Stroke::new(width, color);
    match icon {
        Icon::Search => {
            let circle_center = center - Vec2::splat(size * 0.08);
            let radius = size * 0.27;
            painter.circle_stroke(circle_center, radius, stroke);
            let diagonal = Vec2::splat(size * 0.22);
            painter.line_segment(
                [
                    circle_center + diagonal * 0.72,
                    circle_center + diagonal * 1.35,
                ],
                stroke,
            );
        }
        Icon::Refresh | Icon::Retry => {
            let radius = size * 0.32;
            let start = -0.65_f32;
            let end = 4.85_f32;
            let points = (0..=24)
                .map(|index| {
                    let angle = start + (end - start) * index as f32 / 24.0;
                    center + Vec2::angled(angle) * radius
                })
                .collect::<Vec<_>>();
            painter.add(egui::Shape::line(points.clone(), stroke));
            let tip = *points.last().unwrap_or(&center);
            painter.line_segment([tip, tip + Vec2::new(-size * 0.02, -size * 0.22)], stroke);
            painter.line_segment([tip, tip + Vec2::new(size * 0.2, -size * 0.04)], stroke);
        }
        Icon::Settings => {
            for (offset, knob) in [(-0.28, -0.12), (0.0, 0.17), (0.28, -0.02)] {
                let y = center.y + size * offset;
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    stroke,
                );
                painter.circle_filled(Pos2::new(center.x + size * knob, y), width * 1.8, color);
            }
        }
        Icon::Download => {
            painter.line_segment(
                [
                    Pos2::new(center.x, rect.top()),
                    Pos2::new(center.x, center.y + size * 0.12),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - size * 0.22, center.y - size * 0.02),
                    Pos2::new(center.x, center.y + size * 0.2),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x, center.y + size * 0.2),
                    Pos2::new(center.x + size * 0.22, center.y - size * 0.02),
                ],
                stroke,
            );
            let tray_y = rect.bottom() - size * 0.08;
            painter.line_segment(
                [
                    Pos2::new(rect.left(), tray_y),
                    Pos2::new(rect.right(), tray_y),
                ],
                stroke,
            );
        }
        Icon::Close => {
            painter.line_segment([rect.left_top(), rect.right_bottom()], stroke);
            painter.line_segment([rect.right_top(), rect.left_bottom()], stroke);
        }
        Icon::Play => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(rect.left() + size * 0.16, rect.top()),
                    Pos2::new(rect.right(), center.y),
                    Pos2::new(rect.left() + size * 0.16, rect.bottom()),
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            let offset = size * 0.18;
            let pause_stroke = Stroke::new(width + 1.2, color);
            painter.line_segment(
                [
                    Pos2::new(center.x - offset, rect.top()),
                    Pos2::new(center.x - offset, rect.bottom()),
                ],
                pause_stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x + offset, rect.top()),
                    Pos2::new(center.x + offset, rect.bottom()),
                ],
                pause_stroke,
            );
        }
        Icon::Queue => {
            for offset in [-0.28, 0.0, 0.28] {
                let y = center.y + size * offset;
                painter.circle_filled(Pos2::new(rect.left() + width, y), width * 1.25, color);
                painter.line_segment(
                    [
                        Pos2::new(rect.left() + size * 0.22, y),
                        Pos2::new(rect.right(), y),
                    ],
                    stroke,
                );
            }
        }
        Icon::Check => {
            painter.line_segment(
                [
                    Pos2::new(rect.left(), center.y),
                    Pos2::new(center.x - size * 0.08, rect.bottom()),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    Pos2::new(center.x - size * 0.08, rect.bottom()),
                    Pos2::new(rect.right(), rect.top()),
                ],
                stroke,
            );
        }
        Icon::Folder => {
            let folder = Rect::from_min_max(
                Pos2::new(rect.left(), rect.top() + size * 0.18),
                rect.right_bottom(),
            );
            painter.rect_stroke(folder, 2.0, stroke, egui::StrokeKind::Inside);
            painter.line_segment(
                [
                    folder.left_top(),
                    Pos2::new(folder.left() + size * 0.32, folder.top()),
                ],
                stroke,
            );
        }
        Icon::User => {
            painter.circle_stroke(
                Pos2::new(center.x, rect.top() + size * 0.28),
                size * 0.2,
                stroke,
            );
            let points = (0..=16)
                .map(|index| {
                    let angle = std::f32::consts::PI + std::f32::consts::PI * index as f32 / 16.0;
                    Pos2::new(center.x, rect.bottom() + size * 0.04)
                        + Vec2::angled(angle) * size * 0.38
                })
                .collect::<Vec<_>>();
            painter.add(egui::Shape::line(points, stroke));
        }
        Icon::Alert => {
            painter.circle_stroke(center, size * 0.43, stroke);
            painter.line_segment(
                [
                    Pos2::new(center.x, rect.top() + size * 0.2),
                    Pos2::new(center.x, center.y + size * 0.08),
                ],
                stroke,
            );
            painter.circle_filled(
                Pos2::new(center.x, rect.bottom() - size * 0.2),
                width * 0.9,
                color,
            );
        }
        Icon::Image => {
            painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
            painter.circle_filled(
                Pos2::new(rect.right() - size * 0.25, rect.top() + size * 0.26),
                size * 0.08,
                color,
            );
            painter.add(egui::Shape::line(
                vec![
                    Pos2::new(rect.left() + size * 0.08, rect.bottom() - size * 0.12),
                    Pos2::new(center.x - size * 0.08, center.y),
                    Pos2::new(center.x + size * 0.1, center.y + size * 0.16),
                    Pos2::new(rect.right() - size * 0.08, rect.bottom() - size * 0.12),
                ],
                stroke,
            ));
        }
    }
}

fn configure_style(ctx: &egui::Context) {
    configure_fonts(ctx);
    ctx.set_theme(egui::Theme::Dark);
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    style.animation_time = if system_animations_enabled() {
        0.18
    } else {
        0.0
    };
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.interact_size = Vec2::new(40.0, 36.0);
    style.spacing.window_margin = egui::Margin::same(24);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.interaction.tooltip_delay = 0.35;
    style.visuals.dark_mode = true;
    style.visuals.override_text_color = Some(TEXT_PRIMARY);
    style.visuals.weak_text_color = Some(TEXT_MUTED);
    style.visuals.panel_fill = APP_BG;
    style.visuals.window_fill = SURFACE;
    style.visuals.window_stroke = Stroke::new(1.0, BORDER_STRONG);
    style.visuals.window_corner_radius = 16.into();
    style.visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 12],
        blur: 32,
        spread: 0,
        color: Color32::from_black_alpha(150),
    };
    style.visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 8],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha(145),
    };
    style.visuals.menu_corner_radius = 10.into();
    style.visuals.extreme_bg_color = SURFACE_RAISED;
    style.visuals.text_edit_bg_color = Some(SURFACE_RAISED);
    style.visuals.faint_bg_color = SURFACE_RAISED;
    style.visuals.code_bg_color = SURFACE_RAISED;
    style.visuals.hyperlink_color = ACCENT_HOVER;
    style.visuals.warn_fg_color = WARNING;
    style.visuals.error_fg_color = DANGER;
    style.visuals.selection.bg_fill = ACCENT;
    style.visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    style.visuals.text_cursor.stroke = Stroke::new(1.5, ACCENT_HOVER);
    style.visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    style.visuals.disabled_alpha = 0.42;
    style.visuals.widgets.noninteractive.bg_fill = SURFACE;
    style.visuals.widgets.noninteractive.weak_bg_fill = SURFACE;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    style.visuals.widgets.noninteractive.corner_radius = 9.into();
    style.visuals.widgets.inactive.bg_fill = SURFACE_RAISED;
    style.visuals.widgets.inactive.weak_bg_fill = SURFACE_RAISED;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    style.visuals.widgets.inactive.corner_radius = 9.into();
    style.visuals.widgets.hovered.bg_fill = SURFACE_HOVER;
    style.visuals.widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    style.visuals.widgets.hovered.corner_radius = 9.into();
    style.visuals.widgets.active.bg_fill = ACCENT_MUTED;
    style.visuals.widgets.active.weak_bg_fill = ACCENT_MUTED;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_HOVER);
    style.visuals.widgets.active.corner_radius = 9.into();
    style.visuals.widgets.open = style.visuals.widgets.active;
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn configure_fonts(ctx: &egui::Context) {
    let windows_dir = std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
    let font_path = windows_dir.join("Fonts").join("segoeui.ttf");
    let Ok(bytes) = std::fs::read(font_path) else {
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Segoe UI".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Segoe UI".to_owned());
    ctx.set_fonts(fonts);
}

fn system_animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };

    let mut enabled = 1_i32;
    let result = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut enabled as *mut i32).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    result.is_err() || enabled != 0
}

fn mode_label(mode: &str) -> &str {
    match mode {
        "osu" => "osu!",
        "taiko" => "Taiko",
        "catch" => "Catch",
        "mania" => "Mania",
        _ => "All modes",
    }
}

fn status_label(status: &str) -> &str {
    match status {
        "ranked" => "Ranked",
        "qualified" => "Qualified",
        "loved" => "Loved",
        "pending" => "Pending",
        "graveyard" => "Graveyard",
        "any" => "Any status",
        _ => status,
    }
}

fn status_color(status: &str) -> Color32 {
    match status {
        "ranked" => SUCCESS,
        "qualified" => Color32::from_rgb(197, 216, 109),
        "loved" => ACCENT_HOVER,
        "pending" => WARNING,
        "graveyard" => TEXT_MUTED,
        _ => TEXT_SECONDARY,
    }
}

fn status_background(status: &str) -> Color32 {
    match status {
        "ranked" => Color32::from_rgb(23, 45, 37),
        "qualified" => Color32::from_rgb(42, 45, 25),
        "loved" => ACCENT_MUTED,
        "pending" => Color32::from_rgb(47, 37, 23),
        "graveyard" => SURFACE_RAISED,
        _ => SURFACE_RAISED,
    }
}

fn download_status_color(status: DownloadStatus) -> Color32 {
    match status {
        DownloadStatus::Queued => TEXT_SECONDARY,
        DownloadStatus::Downloading => INFO,
        DownloadStatus::Extracting => WARNING,
        DownloadStatus::Completed => SUCCESS,
        DownloadStatus::Failed => DANGER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_mode_icons_are_valid_images() {
        for icon in [
            MODE_OSU_ICON,
            MODE_TAIKO_ICON,
            MODE_CATCH_ICON,
            MODE_MANIA_ICON,
        ] {
            assert!(image::load_from_memory(icon).is_ok());
        }
    }

    #[test]
    fn jpeg_cover_decoder_is_enabled() {
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode(&[255, 102, 171], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(image::load_from_memory(&encoded).is_ok());
    }
}
