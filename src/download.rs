use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError};
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
const MAX_ARCHIVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Importing,
    Completed,
    Failed,
}

impl DownloadStatus {
    pub fn text(self, progress: f32) -> String {
        match self {
            Self::Queued => "queued".to_owned(),
            Self::Downloading => format!("{progress:.0}%"),
            Self::Importing => "importing...".to_owned(),
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
    #[error("downloaded archive exceeds the 2 GiB safety limit")]
    ArchiveTooLarge,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("could not open the archive in osu!: {0}")]
    OsuLaunch(std::io::Error),
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
    revision: u64,
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
            revision: 0,
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

    /// Records a completed download. Returns whether the timestamps changed and need persisting.
    fn commit(&mut self, reservation: RateReservation) -> bool {
        if !reservation.counted {
            return false;
        }
        self.reservations = self.reservations.saturating_sub(1);
        self.timestamps.push(Utc::now());
        self.timestamps.sort_unstable();
        self.dirty = true;
        self.revision += 1;
        true
    }

    fn needs_persist_retry(&self) -> bool {
        self.dirty
            && self
                .last_persist_attempt
                .is_none_or(|attempt| attempt.elapsed() >= Duration::from_secs(5))
    }

    fn snapshot(&mut self) -> RateLimitSnapshot {
        self.prune();
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

    /// Returns the data to write when unsaved changes exist; the write happens outside the lock.
    fn persist_payload(&mut self) -> Option<(PathBuf, Vec<DateTime<Utc>>, u64)> {
        if !self.dirty {
            return None;
        }
        self.last_persist_attempt = Some(std::time::Instant::now());
        Some((self.path.clone(), self.timestamps.clone(), self.revision))
    }

    fn persisted(&mut self, revision: u64) {
        if self.revision == revision {
            self.dirty = false;
        }
    }
}

#[derive(Clone)]
pub struct DownloadService {
    client: Client,
    paths: DataPaths,
    osu_executable: Arc<RwLock<Option<PathBuf>>>,
    semaphore: Arc<Semaphore>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    rate_limit_persist: Arc<Mutex<()>>,
    events: mpsc::UnboundedSender<DownloadEvent>,
}

impl DownloadService {
    pub fn new(
        paths: DataPaths,
        settings: &AppSettings,
        events: mpsc::UnboundedSender<DownloadEvent>,
    ) -> Result<Self, reqwest::Error> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(30))
            .user_agent(concat!(
                "OsuBmDownloader/",
                env!("CARGO_PKG_VERSION"),
                "-rust"
            ))
            .build()?;
        sweep_partial_downloads(&paths.temp_songs_dir);
        let limiter = RateLimiter::load(settings.is_supporter, paths.rate_limit_file.clone());
        Ok(Self {
            client,
            paths,
            osu_executable: Arc::new(RwLock::new(settings.osu_executable_path())),
            semaphore: Arc::new(Semaphore::new(2)),
            rate_limiter: Arc::new(Mutex::new(limiter)),
            rate_limit_persist: Arc::new(Mutex::new(())),
            events,
        })
    }

    pub fn update_settings(&self, settings: &AppSettings) {
        *self
            .osu_executable
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings.osu_executable_path();
        self.lock_rate_limiter()
            .set_supporter(settings.is_supporter);
        self.emit_rate_limit();
    }

    pub fn rate_limit(&self) -> RateLimitSnapshot {
        let (snapshot, retry_persist) = {
            let mut limiter = self.lock_rate_limiter();
            (limiter.snapshot(), limiter.needs_persist_retry())
        };
        if retry_persist && let Err(error) = self.persist_rate_limit(false) {
            tracing::warn!(%error, "could not retry download rate-limit persistence");
        }
        snapshot
    }

    pub fn spawn(
        self: &Arc<Self>,
        runtime: &Handle,
        attempt_id: u64,
        request: DownloadRequest,
        cancellation: CancellationToken,
    ) -> Result<(), RateLimitSnapshot> {
        let reservation = self.lock_rate_limiter().reserve();
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
        let Ok(permit) = permit else {
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
                drop(permit);
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
        let mut downloaded = false;
        if !archive_path.exists() {
            let _ = self.events.send(DownloadEvent::Status {
                id: request.beatmap_set_id,
                attempt_id,
                status: DownloadStatus::Downloading,
            });
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
                        if let Err(error) = tokio::fs::rename(&partial_path, &archive_path).await {
                            let _ = tokio::fs::remove_file(&partial_path).await;
                            return Err(error.into());
                        }
                        downloaded = true;
                        break;
                    }
                    Ok(false)
                    | Err(DownloadError::Timeout)
                    | Err(DownloadError::Http(_))
                    | Err(DownloadError::ArchiveTooLarge)
                    | Err(DownloadError::Network(_)) => {}
                    Err(error) => {
                        let _ = tokio::fs::remove_file(&partial_path).await;
                        return Err(error);
                    }
                }
            }
            let _ = tokio::fs::remove_file(&partial_path).await;
            if !downloaded {
                return Err(DownloadError::AllMirrorsFailed);
            }
        }

        let reservation = reservation
            .take()
            .expect("reservation is settled exactly once");
        if downloaded {
            self.commit_reservation(reservation);
        } else {
            // A cached archive was reused; no mirror download consumed a rate-limit slot.
            self.release_reservation(reservation);
        }

        if request.auto_install {
            let executable = self
                .osu_executable
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if let Some(executable) = executable {
                let _ = self.events.send(DownloadEvent::Status {
                    id: request.beatmap_set_id,
                    attempt_id,
                    status: DownloadStatus::Importing,
                });
                open_in_osu(&executable, &archive_path)?;
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
        self.lock_rate_limiter().release(reservation);
    }

    fn commit_reservation(&self, reservation: RateReservation) {
        let changed = self.lock_rate_limiter().commit(reservation);
        if changed && let Err(error) = self.persist_rate_limit(true) {
            tracing::error!(%error, "could not persist download rate limit");
        }
        self.emit_rate_limit();
    }

    /// Writes the rate-limit timestamps without holding the limiter lock during file I/O.
    /// Writers are serialized so an older snapshot never overwrites a newer one on disk.
    fn persist_rate_limit(&self, wait: bool) -> anyhow::Result<()> {
        let _guard = if wait {
            self.rate_limit_persist
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        } else {
            match self.rate_limit_persist.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return Ok(()),
            }
        };
        let Some((path, timestamps, revision)) = self.lock_rate_limiter().persist_payload() else {
            return Ok(());
        };
        storage::write_encrypted_json(&path, &timestamps)?;
        self.lock_rate_limiter().persisted(revision);
        Ok(())
    }

    fn lock_rate_limiter(&self) -> MutexGuard<'_, RateLimiter> {
        self.rate_limiter
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    if let Some(songs_path) = osu_songs_path
        && let Ok(entries) = std::fs::read_dir(songs_path)
    {
        for entry in entries.flatten() {
            let Some(id) = id_from_file_name(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            if ids.contains(&id) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let has_file = std::fs::read_dir(entry.path()).is_ok_and(|children| {
                children.flatten().any(|child| {
                    Path::new(&child.file_name())
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("osu"))
                        && child.file_type().is_ok_and(|kind| kind.is_file())
                })
            });
            if has_file {
                ids.insert(id);
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&paths.temp_songs_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("osz"))
                && let Some(id) = id_from_file_name(&name.to_string_lossy())
            {
                ids.insert(id);
            }
        }
    }
    ids
}

fn sweep_partial_downloads(directory: &Path) {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(%error, directory = %directory.display(), "could not scan for partial downloads");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("part"))
            && let Err(error) = std::fs::remove_file(&path)
        {
            tracing::warn!(%error, path = %path.display(), "could not remove partial download");
        }
    }
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
        "{} {} - {}{}.osz",
        request.beatmap_set_id,
        sanitize_file_name(&request.artist),
        sanitize_file_name(&request.title),
        if request.no_video { " [no video]" } else { "" }
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

/// Hands the archive to osu!, which imports it itself (or forwards it to an already running instance).
/// When osu!.exe is not next to the Songs folder (e.g. a relocated Songs directory), the archive is
/// opened through its Windows file association instead.
fn open_in_osu(executable: &Path, archive_path: &Path) -> Result<(), DownloadError> {
    let archive_path = std::path::absolute(archive_path)?;
    if !executable.is_file() {
        return open::that(&archive_path).map_err(DownloadError::OsuLaunch);
    }
    let mut command = std::process::Command::new(executable);
    command.arg(archive_path);
    if let Some(directory) = executable.parent() {
        command.current_dir(directory);
    }
    command.spawn().map_err(DownloadError::OsuLaunch)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn dirty_rate_limit_is_not_replaced_during_role_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing").join("rate.dat");
        let mut limiter = RateLimiter::load(false, path);
        let reservation = limiter.reserve().unwrap();
        assert!(limiter.commit(reservation));
        assert!(limiter.dirty);

        limiter.set_supporter(true);
        limiter.set_supporter(false);
        assert_eq!(limiter.timestamps.len(), 1);
    }

    #[test]
    fn stale_persist_does_not_clear_newer_changes() {
        let directory = tempfile::tempdir().unwrap();
        let mut limiter = RateLimiter::load(false, directory.path().join("rate.dat"));
        let first = limiter.reserve().unwrap();
        let second = limiter.reserve().unwrap();
        assert!(limiter.commit(first));
        let (_, _, revision) = limiter.persist_payload().unwrap();
        assert!(limiter.commit(second));
        limiter.persisted(revision);
        assert!(limiter.dirty);
        let (_, timestamps, revision) = limiter.persist_payload().unwrap();
        assert_eq!(timestamps.len(), 2);
        limiter.persisted(revision);
        assert!(!limiter.dirty);
        assert!(limiter.persist_payload().is_none());
    }

    #[test]
    fn no_video_archives_are_named_separately() {
        let mut request = DownloadRequest {
            beatmap_set_id: 42,
            title: "T".to_owned(),
            artist: "A".to_owned(),
            no_video: false,
            auto_install: true,
        };
        let with_video = archive_name(&request);
        request.no_video = true;
        let without_video = archive_name(&request);
        assert_ne!(with_video, without_video);
        assert_eq!(id_from_file_name(&with_video), Some(42));
        assert_eq!(id_from_file_name(&without_video), Some(42));
    }
}
