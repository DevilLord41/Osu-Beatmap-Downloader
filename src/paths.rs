use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct DataPaths {
    pub data_dir: PathBuf,
    pub settings_file: PathBuf,
    pub rate_limit_file: PathBuf,
    pub download_queue_file: PathBuf,
    pub search_cache_file: PathBuf,
    pub debug_log_file: PathBuf,
    pub temp_songs_dir: PathBuf,
    pub preview_cache_dir: PathBuf,
}

impl DataPaths {
    pub fn discover() -> Result<Self> {
        let data_dir = if let Some(path) = std::env::var_os("OSU_BM_DATA_DIR") {
            PathBuf::from(path)
        } else {
            let executable = std::env::current_exe().context("locate application executable")?;
            executable
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("data")
        };
        Self::from_data_dir(data_dir)
    }

    pub fn from_data_dir(data_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("create data directory {}", data_dir.display()))?;
        let temp_songs_dir = data_dir.join("_temp_songs");
        let preview_cache_dir = data_dir.join("_preview_cache");
        std::fs::create_dir_all(&temp_songs_dir)?;
        std::fs::create_dir_all(&preview_cache_dir)?;

        Ok(Self {
            settings_file: data_dir.join("settings.dat"),
            rate_limit_file: data_dir.join("rate_limit.dat"),
            download_queue_file: data_dir.join("download_queue.dat"),
            search_cache_file: data_dir.join("search_cache.dat"),
            debug_log_file: data_dir.join("debug.log"),
            temp_songs_dir,
            preview_cache_dir,
            data_dir,
        })
    }
}
