#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use osu_beatmap_downloader::app::BeatmapApp;
use osu_beatmap_downloader::paths::DataPaths;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone)]
struct LogWriter(PathBuf);

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = Box<dyn Write + Send>;

    fn make_writer(&'a self) -> Self::Writer {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .map(|file| Box::new(file) as Box<dyn Write + Send>)
            .unwrap_or_else(|_| Box::new(io::sink()))
    }
}

fn main() -> eframe::Result {
    let log_path = DataPaths::discover()
        .map(|paths| paths.debug_log_file)
        .unwrap_or_else(|_| PathBuf::from("debug.log"));
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "osu_beatmap_downloader=info".into()),
        )
        .with_ansi(false)
        .with_target(false)
        .with_writer(LogWriter(log_path.clone()))
        .init();
    install_panic_hook(log_path.clone());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("osu! Beatmap Downloader")
            .with_inner_size([1240.0, 780.0])
            .with_min_inner_size([980.0, 600.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "osu! Beatmap Downloader",
        options,
        Box::new(|creation| {
            BeatmapApp::new(creation)
                .map(|app| Box::new(app) as Box<dyn eframe::App>)
                .map_err(|error| error.into())
        }),
    );
    if let Err(error) = &result {
        append_fatal_error(&log_path, &error.to_string());
        let _ = rfd::MessageDialog::new()
            .set_title("osu! Beatmap Downloader")
            .set_description(format!("The application could not start:\n\n{error}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    result
}

fn install_panic_hook(log_path: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        append_fatal_error(&log_path, &format!("panic: {info}"));
        previous(info);
    }));
}

fn append_fatal_error(path: &PathBuf, message: &str) {
    if let Ok(mut file) = File::options().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
    }
}
