use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

use crate::models::AppSettings;
use crate::paths::DataPaths;
use crate::storage;

const RATE_LIMIT_COUNT: usize = 30;
const RATE_LIMIT_HOURS: i64 = 1;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_UNCOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Extracting,
    Completed,
    Failed,
}

impl DownloadStatus {
    pub fn text(self, progress: f32) -> String {
        match self {
            Self::Queued => "queued".to_owned(),
            Self::Downloading => format!("{progress:.0}%"),
            Self::Extracting => "extracting...".to_owned(),
            Self::Completed => "done".to_owned(),
            Self::Failed => "failed".to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub beatmap_set_id: i32,
    pub title: String,
    pub artist: String,
    pub no_video: bool,
    pub auto_install: bool,
}

#[derive(Debug, Clone)]
pub struct DownloadSnapshot {
    pub attempt_id: u64,
    pub request: DownloadRequest,
    pub status: DownloadStatus,
    pub progress: f32,
    pub error: Option<String>,
}

impl DownloadSnapshot {
    pub fn display_name(&self) -> String {
        format!("{} - {}", self.request.artist, self.request.title)
    }
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Status {
        id: i32,
        attempt_id: u64,
        status: DownloadStatus,
    },
    Progress {
        id: i32,
        attempt_id: u64,
        percent: f32,
    },
    Completed {
        id: i32,
        attempt_id: u64,
    },
    Failed {
        id: i32,
        attempt_id: u64,
        error: String,
    },
    Cancelled {
        id: i32,
        attempt_id: u64,
    },
    Removed {
        id: i32,
        attempt_id: u64,
    },
    RateLimit(RateLimitSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub unlimited: bool,
    pub remaining: usize,
    pub cooldown_seconds: u64,
}

impl RateLimitSnapshot {
    pub fn text(&self) -> String {
        if self.unlimited {
            return "Unlimited".to_owned();
        }
        if self.cooldown_seconds > 0 {
            format!("Next download in {}s", self.cooldown_seconds)
        } else {
            format!("{}/{} available", self.remaining, RATE_LIMIT_COUNT)
        }
    }
}

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("download cancelled")]
    Cancelled,
    #[error("all download mirrors failed")]
    AllMirrorsFailed,
    #[error("download timed out")]
    Timeout,
    #[error("server returned HTTP {0}")]
    Http(StatusCode),
    #[error("downloaded file is not a valid .osz archive")]
    InvalidArchive,
    #[error("downloaded archive exceeds the 2 GiB safety limit")]
    ArchiveTooLarge,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("archive error: {0}")]
    Archive(#[from] zip::result::ZipError),
    #[error("background task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct QueueEntry {
    pub beatmap_set_id: i32,
    pub title: String,
    pub artist: String,
    pub no_video: bool,
    pub auto_install: bool,
}

impl Default for QueueEntry {
    fn default() -> Self {
        Self {
            beatmap_set_id: 0,
            title: String::new(),
            artist: String::new(),
            no_video: false,
            auto_install: true,
        }
    }
}

impl From<&DownloadSnapshot> for QueueEntry {
    fn from(value: &DownloadSnapshot) -> Self {
        Self {
            beatmap_set_id: value.request.beatmap_set_id,
            title: value.request.title.clone(),
            artist: value.request.artist.clone(),
            no_video: value.request.no_video,
            auto_install: value.request.auto_install,
        }
    }
}

impl From<QueueEntry> for DownloadRequest {
    fn from(value: QueueEntry) -> Self {
        Self {
            beatmap_set_id: value.beatmap_set_id,
            title: value.title,
            artist: value.artist,
            no_video: value.no_video,
            auto_install: value.auto_install,
        }
    }
}

struct RateLimiter {
    supporter: bool,
    timestamps: Vec<DateTime<Utc>>,
    reservations: usize,
    path: PathBuf,
    dirty: bool,
    last_persist_attempt: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Copy)]
struct RateReservation {
    counted: bool,
}

impl RateLimiter {
    fn load(supporter: bool, path: PathBuf) -> Self {
        let timestamps = if supporter {
            Vec::new()
        } else {
            storage::read_encrypted_json(&path)
                .ok()
                .flatten()
                .unwrap_or_default()
        };
        let mut limiter = Self {
            supporter,
            timestamps,
            reservations: 0,
            path,
            dirty: false,
            last_persist_attempt: None,
        };
        limiter.prune();
        limiter
    }

    fn set_supporter(&mut self, supporter: bool) {
        if self.supporter == supporter {
            return;
        }
        self.supporter = supporter;
        if !supporter && !self.dirty {
            self.timestamps = storage::read_encrypted_json(&self.path)
                .ok()
                .flatten()
                .unwrap_or_default();
            self.prune();
        }
    }

    fn reserve(&mut self) -> Option<RateReservation> {
        self.prune();
        if self.supporter {
            return Some(RateReservation { counted: false });
        }
        if self.timestamps.len() + self.reservations >= RATE_LIMIT_COUNT {
            return None;
        }
        self.reservations += 1;
        Some(RateReservation { counted: true })
    }

    fn release(&mut self, reservation: RateReservation) {
        if reservation.counted {
            self.reservations = self.reservations.saturating_sub(1);
        }
    }

    fn commit(&mut self, reservation: RateReservation) -> anyhow::Result<()> {
        if !reservation.counted {
            return Ok(());
        }
        self.reservations = self.reservations.saturating_sub(1);
        self.timestamps.push(Utc::now());
        self.timestamps.sort_unstable();
        self.dirty = true;
        self.persist()
    }

    fn snapshot(&mut self) -> RateLimitSnapshot {
        self.prune();
        if self.dirty
            && self
                .last_persist_attempt
                .is_none_or(|attempt| attempt.elapsed() >= Duration::from_secs(5))
            && let Err(error) = self.persist()
        {
            tracing::warn!(%error, "could not retry download rate-limit persistence");
        }
        if self.supporter {
            return RateLimitSnapshot {
                unlimited: true,
                remaining: usize::MAX,
                cooldown_seconds: 0,
            };
        }
        let used = self.timestamps.len() + self.reservations;
        let remaining = RATE_LIMIT_COUNT.saturating_sub(used);
        let cooldown_seconds = if used >= RATE_LIMIT_COUNT {
            self.timestamps
                .first()
                .map(|oldest| {
                    let ready = *oldest + TimeDelta::hours(RATE_LIMIT_HOURS);
                    (ready - Utc::now()).num_milliseconds().max(0) as u64
                })
                .unwrap_or(0)
                .div_ceil(1000)
        } else {
            0
        };
        RateLimitSnapshot {
            unlimited: false,
            remaining,
            cooldown_seconds,
        }
    }

    fn prune(&mut self) {
        let cutoff = Utc::now() - TimeDelta::hours(RATE_LIMIT_HOURS);
        self.timestamps.retain(|timestamp| *timestamp >= cutoff);
        self.timestamps.sort_unstable();
    }

    fn persist(&mut self) -> anyhow::Result<()> {
        self.last_persist_attempt = Some(std::time::Instant::now());
        storage::write_encrypted_json(&self.path, &self.timestamps)?;
        self.dirty = false;
        Ok(())
    }
}

#[derive(Clone)]
pub struct DownloadService {
    client: Client,
    paths: DataPaths,
    osu_songs_path: Arc<RwLock<Option<PathBuf>>>,
    semaphore: Arc<Semaphore>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    events: mpsc::UnboundedSender<DownloadEvent>,
}

impl DownloadService {
    pub fn new(
        paths: DataPaths,
        settings: &AppSettings,
        events: mpsc::UnboundedSender<DownloadEvent>,
    ) -> Result<Self, reqwest::Error> {
        if let Some(songs_path) = settings.osu_songs_path().filter(|path| path.is_dir())
            && let Err(error) = recover_install_artifacts(&songs_path)
        {
            tracing::warn!(%error, "could not recover interrupted beatmap installation");
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(10 * 60))
            .user_agent(concat!(
                "OsuBmDownloader/",
                env!("CARGO_PKG_VERSION"),
                "-rust"
            ))
            .build()?;
        let limiter = RateLimiter::load(settings.is_supporter, paths.rate_limit_file.clone());
        Ok(Self {
            client,
            paths,
            osu_songs_path: Arc::new(RwLock::new(settings.osu_songs_path())),
            semaphore: Arc::new(Semaphore::new(2)),
            rate_limiter: Arc::new(Mutex::new(limiter)),
            events,
        })
    }

    pub fn update_settings(&self, settings: &AppSettings) {
        if let Some(songs_path) = settings.osu_songs_path().filter(|path| path.is_dir())
            && let Err(error) = recover_install_artifacts(&songs_path)
        {
            tracing::warn!(%error, "could not recover interrupted beatmap installation");
        }
        *self
            .osu_songs_path
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings.osu_songs_path();
        self.rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_supporter(settings.is_supporter);
        self.emit_rate_limit();
    }

    pub fn rate_limit(&self) -> RateLimitSnapshot {
        self.rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot()
    }

    pub fn spawn(
        self: &Arc<Self>,
        runtime: &Handle,
        attempt_id: u64,
        request: DownloadRequest,
        cancellation: CancellationToken,
    ) -> Result<(), RateLimitSnapshot> {
        let reservation = self
            .rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .reserve();
        let Some(reservation) = reservation else {
            return Err(self.rate_limit());
        };
        self.emit_rate_limit();
        let service = Arc::clone(self);
        runtime.spawn(async move {
            service
                .run(attempt_id, request, cancellation, reservation)
                .await;
        });
        Ok(())
    }

    async fn run(
        self: Arc<Self>,
        attempt_id: u64,
        request: DownloadRequest,
        cancellation: CancellationToken,
        reservation: RateReservation,
    ) {
        let id = request.beatmap_set_id;
        let mut reservation = Some(reservation);
        let permit = tokio::select! {
            _ = cancellation.cancelled() => {
                self.release_reservation(reservation.take().expect("reservation is present"));
                let _ = self.events.send(DownloadEvent::Cancelled { id, attempt_id });
                return;
            }
            permit = Arc::clone(&self.semaphore).acquire_owned() => permit,
        };
        let Ok(_permit) = permit else {
            self.release_reservation(reservation.take().expect("reservation is present"));
            return;
        };

        let result = self
            .download_and_install(&request, &cancellation, &mut reservation, attempt_id)
            .await;
        match result {
            Ok(()) => {
                let _ = self.events.send(DownloadEvent::Status {
                    id,
                    attempt_id,
                    status: DownloadStatus::Completed,
                });
                let _ = self.events.send(DownloadEvent::Progress {
                    id,
                    attempt_id,
                    percent: 100.0,
                });
                let _ = self
                    .events
                    .send(DownloadEvent::Completed { id, attempt_id });
                tokio::time::sleep(Duration::from_secs(2)).await;
                let _ = self.events.send(DownloadEvent::Removed { id, attempt_id });
            }
            Err(DownloadError::Cancelled) => {
                if let Some(reservation) = reservation.take() {
                    self.release_reservation(reservation);
                }
                let _ = self
                    .events
                    .send(DownloadEvent::Cancelled { id, attempt_id });
            }
            Err(error) => {
                if let Some(reservation) = reservation.take() {
                    self.release_reservation(reservation);
                }
                tracing::error!(beatmap_set_id = id, %error, "download failed");
                let _ = self.events.send(DownloadEvent::Failed {
                    id,
                    attempt_id,
                    error: error.to_string(),
                });
            }
        }
        self.emit_rate_limit();
    }

    async fn download_and_install(
        &self,
        request: &DownloadRequest,
        cancellation: &CancellationToken,
        reservation: &mut Option<RateReservation>,
        attempt_id: u64,
    ) -> Result<(), DownloadError> {
        if cancellation.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        let file_name = archive_name(request);
        let archive_path = self.paths.temp_songs_dir.join(file_name);
        let partial_path = archive_path.with_extension("osz.part");

        if archive_path.exists() && !is_valid_zip(&archive_path) {
            std::fs::remove_file(&archive_path)?;
        }
        if !archive_path.exists() {
            let _ = self.events.send(DownloadEvent::Status {
                id: request.beatmap_set_id,
                attempt_id,
                status: DownloadStatus::Downloading,
            });
            let mut downloaded = false;
            for url in mirror_urls(request.beatmap_set_id, request.no_video) {
                if cancellation.is_cancelled() {
                    let _ = tokio::fs::remove_file(&partial_path).await;
                    return Err(DownloadError::Cancelled);
                }
                let _ = tokio::fs::remove_file(&partial_path).await;
                match self
                    .download_from_mirror(
                        &url,
                        &partial_path,
                        request.beatmap_set_id,
                        attempt_id,
                        cancellation,
                    )
                    .await
                {
                    Ok(true) => {
                        if archive_path.exists() {
                            let _ = tokio::fs::remove_file(&archive_path).await;
                        }
                        tokio::fs::rename(&partial_path, &archive_path).await?;
                        downloaded = true;
                        break;
                    }
                    Ok(false)
                    | Err(DownloadError::Timeout)
                    | Err(DownloadError::Http(_))
                    | Err(DownloadError::ArchiveTooLarge)
                    | Err(DownloadError::Network(_)) => {}
                    Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
                    Err(error) => return Err(error),
                }
            }
            let _ = tokio::fs::remove_file(&partial_path).await;
            if !downloaded {
                return Err(DownloadError::AllMirrorsFailed);
            }
        }

        self.commit_reservation(
            reservation
                .take()
                .expect("reservation is committed exactly once"),
        );

        if request.auto_install {
            let _ = self.events.send(DownloadEvent::Status {
                id: request.beatmap_set_id,
                attempt_id,
                status: DownloadStatus::Extracting,
            });
            let songs_path = self
                .osu_songs_path
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if let Some(songs_path) = songs_path.filter(|path| path.is_dir()) {
                let archive = archive_path.clone();
                let request = request.clone();
                let cancellation = cancellation.clone();
                tokio::task::spawn_blocking(move || {
                    extract_archive(&archive, &songs_path, &request, &cancellation)
                })
                .await??;
                let _ = tokio::fs::remove_file(&archive_path).await;
            }
        }
        Ok(())
    }

    async fn download_from_mirror(
        &self,
        url: &str,
        partial_path: &Path,
        id: i32,
        attempt_id: u64,
        cancellation: &CancellationToken,
    ) -> Result<bool, DownloadError> {
        let request = self.client.get(url).send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(DownloadError::Cancelled),
            response = request => response?,
        };
        let status = response.status();
        if !status.is_success() && status != StatusCode::FAILED_DEPENDENCY {
            return Err(DownloadError::Http(status));
        }

        let total_bytes = response.content_length();
        if total_bytes.is_some_and(|total| total > MAX_ARCHIVE_BYTES) {
            return Err(DownloadError::ArchiveTooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut file = tokio::fs::File::create(partial_path).await?;
        let mut downloaded = 0_u64;
        let mut last_percent = -1_i32;
        loop {
            let next = tokio::select! {
                _ = cancellation.cancelled() => return Err(DownloadError::Cancelled),
                next = tokio::time::timeout(Duration::from_secs(30), stream.next()) => next,
            };
            let chunk = match next {
                Err(_) => return Err(DownloadError::Timeout),
                Ok(None) => break,
                Ok(Some(Err(error))) => return Err(DownloadError::Network(error)),
                Ok(Some(Ok(chunk))) => chunk,
            };
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            if downloaded > MAX_ARCHIVE_BYTES {
                return Err(DownloadError::ArchiveTooLarge);
            }
            if let Some(total) = total_bytes.filter(|total| *total > 0) {
                let percent = ((downloaded as f64 / total as f64) * 100.0).min(100.0) as f32;
                let rounded = percent.floor() as i32;
                if rounded != last_percent {
                    last_percent = rounded;
                    let _ = self.events.send(DownloadEvent::Progress {
                        id,
                        attempt_id,
                        percent,
                    });
                }
            }
        }
        file.flush().await?;
        drop(file);

        let path = partial_path.to_owned();
        let valid = tokio::task::spawn_blocking(move || is_valid_zip(&path)).await?;
        Ok(valid)
    }

    fn release_reservation(&self, reservation: RateReservation) {
        self.rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .release(reservation);
    }

    fn commit_reservation(&self, reservation: RateReservation) {
        if let Err(error) = self
            .rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .commit(reservation)
        {
            tracing::error!(%error, "could not persist download rate limit");
        }
        self.emit_rate_limit();
    }

    fn emit_rate_limit(&self) {
        let _ = self
            .events
            .send(DownloadEvent::RateLimit(self.rate_limit()));
    }
}

pub fn load_queue(paths: &DataPaths) -> Vec<QueueEntry> {
    storage::read_encrypted_json(&paths.download_queue_file)
        .ok()
        .flatten()
        .unwrap_or_default()
}

pub fn save_queue(
    paths: &DataPaths,
    queue: &[DownloadSnapshot],
    pending: &[QueueEntry],
) -> anyhow::Result<()> {
    let mut entries = queue
        .iter()
        .filter(|item| item.status != DownloadStatus::Completed)
        .map(QueueEntry::from)
        .collect::<Vec<_>>();
    let mut ids = entries
        .iter()
        .map(|entry| entry.beatmap_set_id)
        .collect::<HashSet<_>>();
    entries.extend(
        pending
            .iter()
            .filter(|entry| ids.insert(entry.beatmap_set_id))
            .cloned(),
    );
    storage::write_encrypted_json(&paths.download_queue_file, &entries)
}

pub fn installed_ids(paths: &DataPaths, osu_songs_path: Option<&Path>) -> HashSet<i32> {
    let mut ids = HashSet::new();
    if let Some(songs_path) = osu_songs_path.filter(|path| path.is_dir())
        && let Ok(entries) = std::fs::read_dir(songs_path)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            let has_file = path.read_dir().ok().is_some_and(|entries| {
                entries.flatten().any(|entry| {
                    let path = entry.path();
                    path.is_file()
                        && path
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("osu"))
                })
            });
            if !path.is_dir() || !has_file {
                continue;
            }
            if let Some(id) = id_from_file_name(&entry.file_name().to_string_lossy()) {
                ids.insert(id);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&paths.temp_songs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("osz"))
                && is_valid_zip(&path)
                && let Some(id) = id_from_file_name(&entry.file_name().to_string_lossy())
            {
                ids.insert(id);
            }
        }
    }
    ids
}

fn id_from_file_name(name: &str) -> Option<i32> {
    name.split_once(' ')?.0.parse().ok()
}

fn mirror_urls(id: i32, no_video: bool) -> [String; 3] {
    [
        format!(
            "https://catboy.best/d/{id}{}",
            if no_video { "n" } else { "" }
        ),
        format!(
            "https://api.nerinyan.moe/d/{id}{}",
            if no_video { "?noVideo=true" } else { "" }
        ),
        format!(
            "https://dl.sayobot.cn/beatmaps/download/{}/{id}",
            if no_video { "novideo" } else { "full" }
        ),
    ]
}

fn archive_name(request: &DownloadRequest) -> String {
    format!(
        "{} {} - {}.osz",
        request.beatmap_set_id,
        sanitize_file_name(&request.artist),
        sanitize_file_name(&request.title)
    )
}

fn sanitize_file_name(value: &str) -> String {
    let mut utf16_units = 0;
    let sanitized = value
        .chars()
        .map_while(|character| {
            let units = character.len_utf16();
            if utf16_units + units > 80 {
                return None;
            }
            utf16_units += units;
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                Some('_')
            } else {
                Some(character)
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim().trim_end_matches(['.', ' ']);
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized.to_owned()
    }
}

fn is_valid_zip(path: &Path) -> bool {
    File::open(path)
        .ok()
        .and_then(|file| ZipArchive::new(file).ok())
        .is_some_and(|archive| !archive.is_empty())
}

fn extract_archive(
    archive_path: &Path,
    songs_path: &Path,
    request: &DownloadRequest,
    cancellation: &CancellationToken,
) -> Result<(), DownloadError> {
    let folder_name = format!(
        "{} {} - {}",
        request.beatmap_set_id,
        sanitize_file_name(&request.artist),
        sanitize_file_name(&request.title)
    );
    let target = songs_path.join(&folder_name);
    let nonce = rand::random::<u64>();
    let staging = songs_path.join(format!(".{folder_name}.{nonce}.installing"));
    let backup = songs_path.join(format!(".{folder_name}.{nonce}.backup"));
    std::fs::create_dir(&staging)?;

    let extraction = (|| -> Result<(), DownloadError> {
        let file = File::open(archive_path)?;
        let mut archive = ZipArchive::new(file)?;
        if archive.is_empty() || archive.len() > MAX_ARCHIVE_ENTRIES {
            return Err(DownloadError::InvalidArchive);
        }
        let mut uncompressed_bytes = 0_u64;
        for index in 0..archive.len() {
            if cancellation.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }
            let mut entry = archive.by_index(index)?;
            let Some(relative) = entry.enclosed_name() else {
                return Err(DownloadError::InvalidArchive);
            };
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
                || relative
                    .components()
                    .any(|component| component.as_os_str().to_string_lossy().contains(':'))
            {
                return Err(DownloadError::InvalidArchive);
            }
            uncompressed_bytes = uncompressed_bytes
                .checked_add(entry.size())
                .filter(|total| *total <= MAX_UNCOMPRESSED_BYTES)
                .ok_or(DownloadError::InvalidArchive)?;
            let output = staging.join(relative);
            if entry.is_dir() {
                std::fs::create_dir_all(&output)?;
                continue;
            }
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut output_file = File::create(&output)?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                if cancellation.is_cancelled() {
                    return Err(DownloadError::Cancelled);
                }
                let read = entry.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                output_file.write_all(&buffer[..read])?;
            }
        }
        Ok(())
    })();
    if let Err(error) = extraction {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if cancellation.is_cancelled() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(DownloadError::Cancelled);
    }

    if target.exists()
        && let Err(error) = std::fs::rename(&target, &backup)
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(DownloadError::Io(error));
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        if backup.exists()
            && let Err(restore_error) = std::fs::rename(&backup, &target)
        {
            return Err(DownloadError::Io(std::io::Error::other(format!(
                "install failed ({error}) and restoring {} failed ({restore_error})",
                backup.display()
            ))));
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(DownloadError::Io(error));
    }
    if backup.exists()
        && let Err(error) = std::fs::remove_dir_all(&backup)
    {
        tracing::warn!(path = %backup.display(), %error, "could not remove installation backup");
    }
    Ok(())
}

fn recover_install_artifacts(songs_path: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(songs_path)?.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(without_prefix) = name.strip_prefix('.') else {
            continue;
        };
        let Some((folder_and_nonce, kind)) = without_prefix.rsplit_once('.') else {
            continue;
        };
        if !matches!(kind, "backup" | "installing") {
            continue;
        }
        let Some((folder, nonce)) = folder_and_nonce.rsplit_once('.') else {
            continue;
        };
        if nonce.parse::<u64>().is_err() || folder.is_empty() {
            continue;
        }
        if kind == "installing" {
            std::fs::remove_dir_all(entry.path())?;
            continue;
        }
        let target = songs_path.join(folder);
        if target.exists() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::rename(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    #[test]
    fn mirror_urls_match_existing_clients() {
        assert_eq!(
            mirror_urls(123, true),
            [
                "https://catboy.best/d/123n",
                "https://api.nerinyan.moe/d/123?noVideo=true",
                "https://dl.sayobot.cn/beatmaps/download/novideo/123",
            ]
        );
        assert_eq!(
            mirror_urls(123, false)[2],
            "https://dl.sayobot.cn/beatmaps/download/full/123"
        );
    }

    #[test]
    fn sanitizes_windows_file_names() {
        assert_eq!(sanitize_file_name("a:b/c*?"), "a_b_c__");
        assert_eq!(sanitize_file_name("title. "), "title");
    }

    #[test]
    fn queue_defaults_match_legacy_csharp_model() {
        let entry: QueueEntry =
            serde_json::from_str(r#"{"BeatmapSetId":42,"Title":"T","Artist":"A"}"#).unwrap();
        assert!(!entry.no_video);
        assert!(entry.auto_install);
    }

    #[test]
    fn limiter_accounts_for_queued_reservations() {
        let directory = tempfile::tempdir().unwrap();
        let mut limiter = RateLimiter::load(false, directory.path().join("rate.dat"));
        let mut reservations = Vec::new();
        for _ in 0..RATE_LIMIT_COUNT {
            reservations.push(limiter.reserve().unwrap());
        }
        assert!(limiter.reserve().is_none());
        assert_eq!(limiter.snapshot().remaining, 0);
        limiter.release(reservations.pop().unwrap());
        assert!(limiter.reserve().is_some());
    }

    #[test]
    fn extraction_replaces_existing_folder_only_after_success() {
        let directory = tempfile::tempdir().unwrap();
        let songs = directory.path().join("Songs");
        std::fs::create_dir(&songs).unwrap();
        let target = songs.join("42 Artist - Title");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("old.txt"), b"old").unwrap();
        let archive = directory.path().join("map.osz");
        write_test_archive(&archive, "map.osu", b"osu file format v14");
        let request = DownloadRequest {
            beatmap_set_id: 42,
            title: "Title".to_owned(),
            artist: "Artist".to_owned(),
            no_video: true,
            auto_install: true,
        };

        extract_archive(&archive, &songs, &request, &CancellationToken::new()).unwrap();
        assert!(target.join("map.osu").is_file());
        assert!(!target.join("old.txt").exists());
    }

    #[test]
    fn extraction_rejects_parent_traversal() {
        let directory = tempfile::tempdir().unwrap();
        let songs = directory.path().join("Songs");
        std::fs::create_dir(&songs).unwrap();
        let archive = directory.path().join("map.osz");
        write_test_archive(&archive, "../outside.txt", b"bad");
        let request = DownloadRequest {
            beatmap_set_id: 42,
            title: "Title".to_owned(),
            artist: "Artist".to_owned(),
            no_video: true,
            auto_install: true,
        };

        let result = extract_archive(&archive, &songs, &request, &CancellationToken::new());
        assert!(matches!(result, Err(DownloadError::InvalidArchive)));
        assert!(!directory.path().join("outside.txt").exists());
        assert!(!songs.join("42 Artist - Title").exists());
    }

    #[test]
    fn recovers_interrupted_install_backup() {
        let directory = tempfile::tempdir().unwrap();
        let songs = directory.path().join("Songs");
        std::fs::create_dir(&songs).unwrap();
        let backup = songs.join(".42 Artist - Title.123.backup");
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(backup.join("map.osu"), b"old").unwrap();

        recover_install_artifacts(&songs).unwrap();
        assert!(songs.join("42 Artist - Title/map.osu").is_file());
        assert!(!backup.exists());
    }

    #[test]
    fn dirty_rate_limit_is_not_replaced_during_role_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing").join("rate.dat");
        let mut limiter = RateLimiter::load(false, path);
        let reservation = limiter.reserve().unwrap();
        assert!(limiter.commit(reservation).is_err());
        assert!(limiter.dirty);

        limiter.set_supporter(true);
        limiter.set_supporter(false);
        assert_eq!(limiter.timestamps.len(), 1);
    }

    fn write_test_archive(path: &Path, name: &str, contents: &[u8]) {
        let file = File::create(path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive
            .start_file(name, SimpleFileOptions::default())
            .unwrap();
        archive.write_all(contents).unwrap();
        archive.finish().unwrap();
    }
}
