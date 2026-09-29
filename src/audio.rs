use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::paths::DataPaths;

#[derive(Debug, Clone)]
pub enum AudioEvent {
    Ready { id: i32, path: PathBuf },
    Failed { id: i32, error: String },
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("preview request cancelled")]
    Cancelled,
    #[error("preview server returned HTTP {0}")]
    Http(reqwest::StatusCode),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct AudioService {
    client: reqwest::Client,
    paths: DataPaths,
    events: mpsc::UnboundedSender<AudioEvent>,
}

impl AudioService {
    pub fn new(
        paths: DataPaths,
        events: mpsc::UnboundedSender<AudioEvent>,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(concat!(
                    "OsuBmDownloader/",
                    env!("CARGO_PKG_VERSION"),
                    "-rust"
                ))
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(60))
                .build()?,
            paths,
            events,
        })
    }

    pub fn request_preview(
        self: &Arc<Self>,
        runtime: &Handle,
        id: i32,
        cancellation: CancellationToken,
    ) {
        let service = Arc::clone(self);
        runtime.spawn(async move {
            match service.download_preview(id, &cancellation).await {
                Ok(path) => {
                    let _ = service.events.send(AudioEvent::Ready { id, path });
                }
                Err(AudioError::Cancelled) => {}
                Err(error) => {
                    tracing::warn!(beatmap_set_id = id, %error, "audio preview failed");
                    let _ = service.events.send(AudioEvent::Failed {
                        id,
                        error: error.to_string(),
                    });
                }
            }
        });
    }

    pub fn remove_cache(&self, id: i32) {
        for name in [
            format!("{id}.mp3"),
            format!("{id}.ogg"),
            format!("{id}.mp3.part"),
            format!("{id}.ogg.part"),
        ] {
            let path = self.paths.preview_cache_dir.join(name);
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), %error, "failed to remove preview cache");
            }
        }
    }

    async fn download_preview(
        &self,
        id: i32,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, AudioError> {
        if let Some(path) = cached_preview(&self.paths.preview_cache_dir, id) {
            return Ok(path);
        }
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(AudioError::Cancelled),
            response = self.client.get(format!("https://b.ppy.sh/preview/{id}.mp3")).send() => response?,
        };
        if !response.status().is_success() {
            return Err(AudioError::Http(response.status()));
        }
        let is_ogg = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("ogg"));
        let extension = if is_ogg { "ogg" } else { "mp3" };
        let path = self
            .paths
            .preview_cache_dir
            .join(format!("{id}.{extension}"));
        let temporary = self
            .paths
            .preview_cache_dir
            .join(format!("{id}.{extension}.part"));
        let result = Self::write_preview(response, &temporary, &path, cancellation).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
        }
        result.map(|()| path)
    }

    async fn write_preview(
        response: reqwest::Response,
        temporary: &Path,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), AudioError> {
        let mut file = tokio::fs::File::create(temporary).await?;
        let mut stream = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => return Err(AudioError::Cancelled),
                chunk = stream.next() => chunk,
            };
            match chunk {
                Some(Ok(chunk)) => file.write_all(&chunk).await?,
                Some(Err(error)) => return Err(AudioError::Network(error)),
                None => break,
            }
        }
        file.flush().await?;
        drop(file);
        if cancellation.is_cancelled() {
            return Err(AudioError::Cancelled);
        }
        tokio::fs::rename(temporary, path).await?;
        Ok(())
    }
}

#[derive(Default)]
pub struct AudioPlayer {
    device: Option<MixerDeviceSink>,
    player: Option<Player>,
    current_id: Option<i32>,
}

impl AudioPlayer {
    pub fn current_id(&self) -> Option<i32> {
        self.current_id
    }

    pub fn reconcile(&mut self) {
        if self.player.as_ref().is_some_and(Player::empty) {
            self.stop();
        }
    }

    pub fn toggle(&mut self, id: i32, path: &Path) -> anyhow::Result<bool> {
        if self.current_id == Some(id) {
            self.stop();
            return Ok(false);
        }
        self.play(id, path)?;
        Ok(true)
    }

    pub fn play(&mut self, id: i32, path: &Path) -> anyhow::Result<()> {
        self.stop();
        let device = DeviceSinkBuilder::open_default_sink()?;
        let player = Player::connect_new(device.mixer());
        let file = BufReader::new(File::open(path)?);
        player.append(Decoder::try_from(file)?);
        self.device = Some(device);
        self.player = Some(player);
        self.current_id = Some(id);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(player) = self.player.take() {
            player.stop();
        }
        self.device = None;
        self.current_id = None;
    }
}

pub fn find_local_audio(songs_path: Option<&Path>, id: i32) -> Option<PathBuf> {
    let songs_path = songs_path.filter(|path| path.is_dir())?;
    let prefix = format!("{id} ");
    let folder = std::fs::read_dir(songs_path)
        .ok()?
        .flatten()
        .find(|entry| {
            entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
        })?
        .path();
    find_osu_audio(&folder)
        .or_else(|| find_extension(&folder, "mp3"))
        .or_else(|| find_extension(&folder, "ogg"))
}

/// Resolves the song file named by the `AudioFilename:` header of the first `.osu` file.
fn find_osu_audio(folder: &Path) -> Option<PathBuf> {
    let osu = find_extension(folder, "osu")?;
    let reader = BufReader::new(File::open(osu).ok()?);
    let name = reader.lines().map_while(Result::ok).find_map(|line| {
        line.trim_start_matches('\u{feff}')
            .trim()
            .strip_prefix("AudioFilename:")
            .map(|value| value.trim().to_owned())
    })?;
    let mut components = Path::new(&name).components();
    let is_plain_name = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    let path = folder.join(&name);
    (is_plain_name && path.is_file()).then_some(path)
}

fn find_extension(folder: &Path, extension: &str) -> Option<PathBuf> {
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
}

fn cached_preview(cache: &Path, id: i32) -> Option<PathBuf> {
    ["mp3", "ogg"]
        .into_iter()
        .map(|extension| cache.join(format!("{id}.{extension}")))
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_audio_prefers_mp3_over_ogg() {
        let directory = tempfile::tempdir().unwrap();
        let set = directory.path().join("42 Artist - Title");
        std::fs::create_dir(&set).unwrap();
        std::fs::write(set.join("audio.ogg"), []).unwrap();
        std::fs::write(set.join("audio.mp3"), []).unwrap();
        assert_eq!(
            find_local_audio(Some(directory.path()), 42)
                .unwrap()
                .extension()
                .unwrap(),
            "mp3"
        );
    }

    #[test]
    fn preview_cache_ignores_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("42.mp3.part"), []).unwrap();
        assert_eq!(cached_preview(directory.path(), 42), None);
    }
}
