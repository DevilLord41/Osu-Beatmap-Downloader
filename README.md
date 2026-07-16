# osu! Beatmap Downloader

A fast native Windows desktop app that replicates osu!direct functionality, allowing you to browse, search, filter, and download osu! beatmaps with ease.

Version 2.0 is a complete rework of the original .NET/WPF application in Rust, with an optimized native build and a brand-new modern interface.

Made by [HardRockMania](https://osu.ppy.sh/u/hardrockmania)

## Download

Grab the latest release from the [Releases page](https://github.com/DevilLord41/Osu-Beatmap-Downloader/releases). Extract the zip and run `OsuBmDownloader.exe` — no installation required.

## What's New in 2.0

- Completely rewritten from C#/.NET and WPF in Rust 2024
- Optimized native Windows release with WGPU rendering, thin LTO, and stripped symbols
- Brand-new minimal dark interface with modern controls, compact download cards, and responsive layouts
- Asynchronous API, cover, audio, and download work powered by Tokio
- Safer downloads with mirror fallback, archive limits, and transactional installation
- Backward-compatible DPAPI storage, so existing v1 settings and application state continue to work

## Features

- Browse all downloadable beatmaps from the osu! website with infinite scroll
- Filter by game mode (osu!, taiko, catch, mania) and status (ranked, qualified, loved, pending, graveyard)
- Advanced search with custom filters: `star>=5 & star<=10`, `bpm>=180`, `ar>=9`, `cs>=4`, `od>=8`, `hp>=5`, `length>=120`
- One-click download with download queue (max 2 concurrent)
- Auto-install: extracts .osz and moves to your osu! Songs folder
- Preview audio playback (supports both MP3 and OGG formats)
- Smart caching: beatmap results cached in memory and on disk for instant mode switching
- Qualified maps refresh from the first page so status changes are immediately visible
- Automatic mirror fallback (catboy.best + nerinyan.moe + sayobot)
- Hides already-downloaded beatmaps (scans your osu! Songs folder)
- "Show Downloaded" toggle to see already-downloaded maps
- Download queue persistence (resumes on app restart)
- Encrypted settings storage (Windows DPAPI)

### osu! Supporter Features

- Unlimited downloads (free users: 30 per hour)
- Preview audio on click
- "Download All" button (up to 100 maps at once)
- Supporter account badge

## Prerequisites

- **Windows 10/11** (required for native rendering and DPAPI)
- **osu! API v2 credentials** - You'll need a Client ID and Client Secret

## Getting osu! API Credentials

1. Go to [osu.ppy.sh/home/account/edit](https://osu.ppy.sh/home/account/edit)
2. Scroll down to "OAuth" section
3. Click "New OAuth Application"
4. Set the Application Callback URL to: `http://localhost:7270/callback`
5. Copy your **Client ID** and **Client Secret**

## First Launch Setup

On first launch, a settings dialog will appear. Enter:
- Your **osu! installation path** (e.g., `D:\osu!`)
- Your **Client ID** and **Client Secret** from the osu! API

## Search Filter Syntax

You can combine text search with filters in any order:

| Filter | Example | Description |
|--------|---------|-------------|
| `star` | `star>=5 & star<=10` | Star rating range |
| `bpm` | `bpm>=180` | BPM filter |
| `length` | `length>=120` | Length in seconds |
| `ar` | `ar>=9` | Approach rate |
| `cs` | `cs>=4` | Circle size |
| `od` | `od>=8` | Overall difficulty |
| `hp` | `hp>=5` | HP drain |

Example: `shuniki star>=5 & star<=10` - Search for "shuniki" with 5-10 star maps.

## Tech Stack

- Rust 2024
- eframe/egui with native WGPU rendering
- Tokio + reqwest for asynchronous API and download work
- osu! API v2 (OAuth2)
- Rodio for MP3 and OGG playback
- Windows DPAPI for encrypted, backward-compatible storage

## Building from Source

### 1. Install Rust

Install Rust with [rustup](https://rustup.rs/). The supported toolchain is pinned in `rust-toolchain.toml`.

Verify installation:
```powershell
rustc --version
cargo --version
```

### 2. Clone the repository

```powershell
git clone https://github.com/DevilLord41/Osu-Beatmap-Downloader.git
cd Osu-Beatmap-Downloader
```

### 3. Build and test

```powershell
cargo test --all-targets
cargo build --release
```

### 4. Run

```powershell
cargo run
```

### Release executable

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
```

This produces `target/x86_64-pc-windows-msvc/release/OsuBmDownloader.exe`. Pushing a `v*` tag runs formatting, lint, and tests before publishing the Windows ZIP release.

## Upgrading from v1 (.NET)

Keep the existing `data` folder beside `OsuBmDownloader.exe`. The Rust application reads the same DPAPI-encrypted settings, rate-limit history, download queue, and search cache files, preserving API credentials and application state for the same Windows user.

## License

This project is not affiliated with or endorsed by osu! or ppy Pty Ltd.
