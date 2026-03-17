# osu! Beatmap Downloader

A Windows desktop app that replicates osu!direct functionality, allowing you to browse, search, filter, and download osu! beatmaps with ease.

## Download

Grab the latest release from the [Releases page](https://github.com/DevilLord41/Osu-Beatmap-Downloader/releases). Extract the zip and run `OsuBmDownloader.exe` — no installation required.

## Features

- Browse all downloadable beatmaps from the osu! website with infinite scroll
- Filter by game mode (osu!, taiko, catch, mania) and status (ranked, qualified, loved, pending, graveyard)
- Advanced search with custom filters: `star>=5 & star<=10`, `bpm>=180`, `ar>=9`, `cs>=4`, `od>=8`, `hp>=5`, `length>=120`
- One-click download with download queue (max 2 concurrent)
- Auto-install: extracts .osz and moves to your osu! Songs folder
- Preview audio playback (supports both MP3 and OGG formats)
- Smart caching: beatmap results cached in memory and on disk for instant mode switching
- Automatic mirror fallback (catboy.best + nerinyan.moe + sayobot)
- Hides already-downloaded beatmaps (scans your osu! Songs folder)
- "Show Downloaded" toggle to see already-downloaded maps
- Download queue persistence (resumes on app restart)
- Encrypted settings storage (Windows DPAPI)

### osu! Supporter Features

- Unlimited downloads (free users: 30 per hour)
- Preview audio on click
- "Download All" button (up to 100 maps at once)
- Supporter heart badge

## Prerequisites

- **Windows 10/11** (required for WPF and DPAPI)
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

- C# / .NET 8
- WPF (Windows Presentation Foundation)
- osu! API v2 (OAuth2)
- NAudio + NAudio.Vorbis (audio playback)
- Windows DPAPI (encrypted storage)

## Building from Source

### 1. Install .NET 8 SDK

Download and install from: https://dotnet.microsoft.com/download/dotnet/8.0

Verify installation:
```bash
dotnet --version
```

### 2. Clone the repository

```bash
git clone https://github.com/DevilLord41/Osu-Beatmap-Downloader.git
cd Osu-Beatmap-Downloader
```

### 3. Restore and build

```bash
dotnet restore
dotnet build
```

### 4. Run

```bash
dotnet run --project OsuBmDownloader
```

### Publishing a self-contained release

```bash
dotnet publish OsuBmDownloader/OsuBmDownloader.csproj -c Release -r win-x64 --self-contained -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true -o publish
```

This produces a single `OsuBmDownloader.exe` in the `publish/` folder with all dependencies bundled.

## License

This project is not affiliated with or endorsed by osu! or ppy Pty Ltd.
