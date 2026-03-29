using System.Collections.ObjectModel;
using System.IO;
using System.IO.Compression;
using System.Net.Http;
using System.Windows;
using Newtonsoft.Json;
using OsuBmDownloader.Models;

namespace OsuBmDownloader.Services;

public class DownloadManager
{
    private readonly HttpClient _downloadClient;

    private static HttpClient CreateClient()
    {
        var client = new HttpClient { Timeout = TimeSpan.FromMinutes(10) };
        client.DefaultRequestHeaders.Add("User-Agent", "OsuBmDownloader/1.0");
        return client;
    }

    private readonly SemaphoreSlim _semaphore = new(2, 2);
    private readonly string _songsPath;
    private readonly string? _osuSongsPath;
    private readonly Action<int> _onDownloadCompleted;
    private readonly Action<int>? _onDownloadFailed;

    // Rate limiter for non-supporters
    private readonly bool _isSupporter;
    private readonly List<DateTime> _downloadTimestamps = new();
    private const int RateLimitCount = 30;
    private static readonly TimeSpan RateLimitWindow = TimeSpan.FromHours(1);

    // Persistence (encrypted)
    private static readonly string TimestampsPath = DataPaths.RateLimitFile;
    private static readonly string QueuePath = DataPaths.DownloadQueueFile;

    // Event for UI to update cooldown display
    public event Action? RateLimitChanged;

    public ObservableCollection<DownloadItem> AllItems { get; } = new();

    public int RemainingDownloads
    {
        get
        {
            if (_isSupporter) return -1;
            PruneOldTimestamps();
            return Math.Max(0, RateLimitCount - _downloadTimestamps.Count);
        }
    }

    /// <summary>
    /// Seconds until next download is available. 0 if ready, -1 if unlimited.
    /// </summary>
    public int CooldownSeconds
    {
        get
        {
            if (_isSupporter) return -1;
            PruneOldTimestamps();
            if (_downloadTimestamps.Count < RateLimitCount) return 0;
            var oldest = _downloadTimestamps[0];
            var ready = oldest + RateLimitWindow;
            var remaining = ready - DateTime.UtcNow;
            return remaining.TotalSeconds > 0 ? (int)Math.Ceiling(remaining.TotalSeconds) : 0;
        }
    }

    public string RateLimitText
    {
        get
        {
            if (_isSupporter) return "Unlimited";
            var remaining = RemainingDownloads;
            var cooldown = CooldownSeconds;
            if (cooldown > 0)
                return $"Next download in {cooldown}s";
            return $"{remaining}/{RateLimitCount} available";
        }
    }

    public DownloadManager(string songsPath, string? osuSongsPath, Action<int> onDownloadCompleted, Action<int>? onDownloadFailed, bool isSupporter)
    {
        _downloadClient = CreateClient();
        _songsPath = songsPath;
        _osuSongsPath = osuSongsPath;
        _onDownloadCompleted = onDownloadCompleted;
        _onDownloadFailed = onDownloadFailed;
        _isSupporter = isSupporter;
        Directory.CreateDirectory(_songsPath);
        LoadTimestamps();
    }

    private void LoadTimestamps()
    {
        if (_isSupporter) return;
        try
        {
            var json = SecureStorage.ReadEncrypted(TimestampsPath);
            if (json != null)
            {
                var saved = JsonConvert.DeserializeObject<List<DateTime>>(json);
                if (saved != null)
                {
                    _downloadTimestamps.AddRange(saved);
                    PruneOldTimestamps();
                }
            }
        }
        catch { /* ignore corrupt file */ }
    }

    private void SaveTimestamps()
    {
        if (_isSupporter) return;
        try
        {
            var json = JsonConvert.SerializeObject(_downloadTimestamps);
            SecureStorage.WriteEncrypted(TimestampsPath, json);
        }
        catch { /* ignore write errors */ }
    }

    private void PruneOldTimestamps()
    {
        var cutoff = DateTime.UtcNow - RateLimitWindow;
        _downloadTimestamps.RemoveAll(t => t < cutoff);
    }

    public bool CanDownload()
    {
        if (_isSupporter) return true;
        PruneOldTimestamps();
        return _downloadTimestamps.Count < RateLimitCount;
    }

    public void Enqueue(BeatmapSet beatmapSet, bool noVideo, bool autoInstall)
    {
        // Remove previous failed entry so re-download works
        var existing = AllItems.FirstOrDefault(i => i.BeatmapSetId == beatmapSet.Id);
        if (existing != null)
        {
            if (existing.Status == DownloadStatus.Failed)
                AllItems.Remove(existing);
            else
                return; // Already queued or downloading
        }

        if (!CanDownload())
        {
            MessageBox.Show(
                $"Download limit reached ({RateLimitCount} per {RateLimitWindow.TotalMinutes:0} min for non-supporters).\nPlease wait or support osu! to get unlimited downloads.",
                "Rate Limited", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }

        var item = new DownloadItem
        {
            BeatmapSetId = beatmapSet.Id,
            Title = beatmapSet.Title,
            Artist = beatmapSet.Artist,
            Status = DownloadStatus.Queued
        };

        AllItems.Add(item);
        Task.Run(() => DownloadItemAsync(item, noVideo, autoInstall));
    }

    public void Retry(DownloadItem item, bool noVideo, bool autoInstall)
    {
        if (!CanDownload())
        {
            MessageBox.Show(
                $"Download limit reached ({RateLimitCount} per {RateLimitWindow.TotalMinutes:0} min for non-supporters).\nPlease wait or support osu! to get unlimited downloads.",
                "Rate Limited", MessageBoxButton.OK, MessageBoxImage.Information);
            return;
        }

        item.Status = DownloadStatus.Queued;
        item.Progress = 0;
        Task.Run(() => DownloadItemAsync(item, noVideo, autoInstall));
    }

    public void Cancel(DownloadItem item)
    {
        item.Cts.Cancel();
        // Don't remove immediately — let the background task clean up via finally block
        // Just mark as failed so user sees feedback
        if (item.Status != DownloadStatus.Completed)
            item.Status = DownloadStatus.Failed;
        Application.Current.Dispatcher.BeginInvoke(() => AllItems.Remove(item));
    }

    public void CancelAll()
    {
        foreach (var item in AllItems.ToList())
        {
            item.Cts.Cancel();
        }
        Application.Current.Dispatcher.BeginInvoke(() => AllItems.Clear());
    }

    private void UpdateUI(Action action)
    {
        Application.Current.Dispatcher.BeginInvoke(action);
    }

    private async Task DownloadItemAsync(DownloadItem item, bool noVideo, bool autoInstall)
    {
        bool semaphoreAcquired = false;
        var ct = item.Cts.Token;
        string? filePath = null;

        try
        {
            await _semaphore.WaitAsync(ct).ConfigureAwait(false);
            semaphoreAcquired = true;

            if (ct.IsCancellationRequested) return;

            var sanitizedArtist = SanitizeFileName(item.Artist);
            var sanitizedTitle = SanitizeFileName(item.Title);
            var fileName = $"{item.BeatmapSetId} {sanitizedArtist} - {sanitizedTitle}.osz";
            filePath = Path.Combine(_songsPath, fileName);

            // Check if valid .osz already exists (e.g. interrupted extraction from previous session)
            // Delete invalid/corrupt files so they get re-downloaded
            if (File.Exists(filePath) && !IsValidZip(filePath))
            {
                try { File.Delete(filePath); } catch { }
            }

            if (!File.Exists(filePath))
            {
                UpdateUI(() => item.Status = DownloadStatus.Downloading);

                // Try mirrors in order: catboy.best (fast), nerinyan.moe, sayobot (fallbacks)
                var urls = new List<string>();

                var catboyUrl = $"https://catboy.best/d/{item.BeatmapSetId}";
                if (noVideo) catboyUrl += "n";
                urls.Add(catboyUrl);

                var nerinyanUrl = $"https://api.nerinyan.moe/d/{item.BeatmapSetId}";
                if (noVideo) nerinyanUrl += "?noVideo=true";
                urls.Add(nerinyanUrl);

                var sayobotUrl = $"https://dl.sayobot.cn/beatmaps/download/{(noVideo ? "novideo" : "full")}/{item.BeatmapSetId}";
                urls.Add(sayobotUrl);

                var downloaded = false;
                foreach (var url in urls)
                {
                    if (ct.IsCancellationRequested) return;

                    try
                    {
                        if (await TryDownloadFromMirror(url, filePath, item, ct))
                        {
                            downloaded = true;
                            break;
                        }
                    }
                    catch (OperationCanceledException) { throw; }
                    catch { /* try next mirror */ }

                    // Clean up failed partial file
                    if (File.Exists(filePath))
                        try { File.Delete(filePath); } catch { }
                }

                if (!downloaded)
                {
                    UpdateUI(() => item.Status = DownloadStatus.Failed);
                    _onDownloadFailed?.Invoke(item.BeatmapSetId);
                    return;
                }
            }

            // Track download for rate limiting
            _downloadTimestamps.Add(DateTime.UtcNow);
            SaveTimestamps();
            RateLimitChanged?.Invoke();

            // Extract .osz and move to osu! Songs folder (if auto install enabled)
            if (autoInstall)
            {
                UpdateUI(() => item.Status = DownloadStatus.Extracting);
                await ExtractAndMoveAsync(filePath, item, ct).ConfigureAwait(false);
            }

            UpdateUI(() =>
            {
                item.Progress = 100;
                item.Status = DownloadStatus.Completed;
            });
            _onDownloadCompleted(item.BeatmapSetId);

            await Task.Delay(2000, ct).ConfigureAwait(false);
            UpdateUI(() => AllItems.Remove(item));
        }
        catch (OperationCanceledException)
        {
            // Cancelled — clean up partial file
            if (filePath != null)
                try { File.Delete(filePath); } catch { }
        }
        catch (Exception ex)
        {
            var logPath = DataPaths.DebugLogFile;
            File.AppendAllText(logPath, $"{DateTime.Now:HH:mm:ss.fff} [DL] FAILED id={item.BeatmapSetId}: {ex.GetType().Name}: {ex.Message}\n");
            UpdateUI(() => item.Status = DownloadStatus.Failed);
            _onDownloadFailed?.Invoke(item.BeatmapSetId);
        }
        finally
        {
            if (semaphoreAcquired)
                _semaphore.Release();
        }
    }

    private async Task ExtractAndMoveAsync(string oszPath, DownloadItem item, CancellationToken ct)
    {
        await Task.Run(() =>
        {
            var destPath = _osuSongsPath;
            if (string.IsNullOrWhiteSpace(destPath) || !Directory.Exists(destPath))
                return; // No osu! Songs path configured — keep .osz in local songs folder

            var sanitizedArtist = SanitizeFileName(item.Artist);
            var sanitizedTitle = SanitizeFileName(item.Title);
            var folderName = $"{item.BeatmapSetId} {sanitizedArtist} - {sanitizedTitle}";
            var targetFolder = Path.Combine(destPath, folderName);

            // Extract .osz (zip) to the osu! Songs folder
            Directory.CreateDirectory(targetFolder);
            ZipFile.ExtractToDirectory(oszPath, targetFolder, overwriteFiles: true);

            // Delete the .osz from local songs folder
            try { File.Delete(oszPath); } catch { }
        }, ct).ConfigureAwait(false);
    }

    // --- Queue persistence ---

    private class QueueEntry
    {
        public int BeatmapSetId { get; set; }
        public string Title { get; set; } = string.Empty;
        public string Artist { get; set; } = string.Empty;
        public bool NoVideo { get; set; }
        public bool AutoInstall { get; set; } = true;
    }

    public void SaveQueue(bool noVideo, bool autoInstall)
    {
        try
        {
            var entries = AllItems
                .Where(i => i.Status != DownloadStatus.Completed)
                .Select(i => new QueueEntry
                {
                    BeatmapSetId = i.BeatmapSetId,
                    Title = i.Title,
                    Artist = i.Artist,
                    NoVideo = noVideo,
                    AutoInstall = autoInstall
                })
                .ToList();

            var json = JsonConvert.SerializeObject(entries);
            SecureStorage.WriteEncrypted(QueuePath, json);
        }
        catch { }
    }

    public void LoadAndResumeQueue()
    {
        try
        {
            var json = SecureStorage.ReadEncrypted(QueuePath);
            if (json == null) return;

            var entries = JsonConvert.DeserializeObject<List<QueueEntry>>(json);
            if (entries == null || entries.Count == 0) return;

            foreach (var entry in entries)
            {
                if (AllItems.Any(i => i.BeatmapSetId == entry.BeatmapSetId))
                    continue;

                var item = new DownloadItem
                {
                    BeatmapSetId = entry.BeatmapSetId,
                    Title = entry.Title,
                    Artist = entry.Artist,
                    Status = DownloadStatus.Queued
                };

                AllItems.Add(item);
                Task.Run(() => DownloadItemAsync(item, entry.NoVideo, entry.AutoInstall));
            }

            // Clear the saved queue now that we've resumed
            try { File.Delete(QueuePath); } catch { }
        }
        catch { }
    }

    private async Task<bool> TryDownloadFromMirror(string url, string filePath, DownloadItem item, CancellationToken ct)
    {
        using var response = await _downloadClient
            .GetAsync(url, HttpCompletionOption.ResponseHeadersRead, ct)
            .ConfigureAwait(false);

        // Accept 2xx and catboy.best 424 quirk
        var accepted = response.IsSuccessStatusCode || (int)response.StatusCode == 424;
        if (!accepted) return false;

        var totalBytes = response.Content.Headers.ContentLength ?? -1;
        using var downloadStream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);

        {
            using var fileStream = File.Create(filePath);
            var buffer = new byte[81920];
            long totalRead = 0;
            int bytesRead;

            while (true)
            {
                // Per-read timeout: if no data received for 30s, abort and try next mirror
                using var readCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
                readCts.CancelAfter(TimeSpan.FromSeconds(30));
                bytesRead = await downloadStream.ReadAsync(buffer, readCts.Token).ConfigureAwait(false);
                if (bytesRead == 0) break;
                await fileStream.WriteAsync(buffer.AsMemory(0, bytesRead), ct).ConfigureAwait(false);
                totalRead += bytesRead;

                if (totalBytes > 0)
                {
                    var progress = (double)totalRead / totalBytes * 100;
                    UpdateUI(() => item.Progress = progress);
                }
            }
        }

        // Validate
        return IsValidZip(filePath);
    }

    private static bool IsValidZip(string path)
    {
        try
        {
            using var zip = ZipFile.OpenRead(path);
            return zip.Entries.Count > 0;
        }
        catch { return false; }
    }

    private static string SanitizeFileName(string name)
    {
        var invalid = Path.GetInvalidFileNameChars();
        return string.Join("", name.Select(c => invalid.Contains(c) ? '_' : c));
    }
}
