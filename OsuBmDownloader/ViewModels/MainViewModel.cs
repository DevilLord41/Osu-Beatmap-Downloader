using System.Collections.ObjectModel;
using System.IO;
using System.Windows;
using System.Windows.Input;
using OsuBmDownloader.Models;
using OsuBmDownloader.Services;

namespace OsuBmDownloader.ViewModels;

public class MainViewModel : BaseViewModel
{
    private static void Log(string msg) => File.AppendAllText(Services.DataPaths.DebugLogFile, $"{DateTime.Now:HH:mm:ss.fff} {msg}\n");

    private readonly OsuApiService _api;
    private readonly DownloadManager _downloadManager;
    private readonly AudioService _audio = new();
    private readonly HashSet<int> _downloadedIds = new();
    private readonly AppSettings _settings;
    private bool _isLoading;
    private CancellationTokenSource _loadCts = new();

    // --- Cache ---
    private class FilterCache
    {
        public List<BeatmapSet> AllResults { get; } = new();
        public string? CursorString { get; set; }
        public bool HasMore { get; set; } = true;
    }

    private readonly Dictionary<string, FilterCache> _cache = new();

    // Persisted cache — saves only cursor positions + seen IDs (lightweight)
    private static readonly string DiskCachePath = Services.DataPaths.SearchCacheFile;

    private class DiskCacheEntry
    {
        public string? CursorString { get; set; }
        public bool HasMore { get; set; } = true;
        public List<BeatmapSet> VisibleResults { get; set; } = new();
    }

    private void LoadDiskCache()
    {
        try
        {
            var json = Services.SecureStorage.ReadEncrypted(DiskCachePath);
            if (json == null) return;
            var entries = Newtonsoft.Json.JsonConvert.DeserializeObject<Dictionary<string, DiskCacheEntry>>(json);
            if (entries == null) return;

            foreach (var (key, entry) in entries)
            {
                var cache = new FilterCache
                {
                    CursorString = entry.CursorString,
                    HasMore = entry.HasMore
                };
                // Restore visible results (non-downloaded maps from previous session)
                cache.AllResults.AddRange(entry.VisibleResults);
                _cache[key] = cache;
            }
        }
        catch { }
    }

    private void SaveDiskCache()
    {
        try
        {
            var toSave = new Dictionary<string, DiskCacheEntry>();
            foreach (var (key, cache) in _cache)
            {
                if (cache.CursorString == null && cache.AllResults.Count == 0) continue;

                toSave[key] = new DiskCacheEntry
                {
                    CursorString = cache.CursorString,
                    HasMore = cache.HasMore,
                    VisibleResults = cache.AllResults
                };
            }
            var json = Newtonsoft.Json.JsonConvert.SerializeObject(toSave);
            Services.SecureStorage.WriteEncrypted(DiskCachePath, json);
        }
        catch { }
    }

    // Cache by API query only (text + mode + status), not client-side filters
    // This lets us reuse API results when only client-side filters change
    private string CacheKey => $"{_activeMode}|{_activeStatus}|{_activeQuery ?? ""}";

    private FilterCache GetOrCreateCache()
    {
        var key = CacheKey;
        if (!_cache.TryGetValue(key, out var cache))
        {
            cache = new FilterCache();
            _cache[key] = cache;
        }
        return cache;
    }

    // Active filter snapshot
    private string _activeMode = "all";
    private string _activeStatus = "ranked";
    private string? _activeQuery;
    private SearchQueryParser _activeFilter = new();

    public ObservableCollection<BeatmapSet> Beatmaps { get; } = new();
    public ObservableCollection<DownloadItem> DownloadQueue => _downloadManager.AllItems;

    private string _selectedMode = "all";
    public string SelectedMode
    {
        get => _selectedMode;
        set
        {
            if (SetField(ref _selectedMode, value))
                _ = ResetAndLoadAsync();
        }
    }

    private string _selectedStatus = "ranked";
    public string SelectedStatus
    {
        get => _selectedStatus;
        set
        {
            if (SetField(ref _selectedStatus, value))
                _ = ResetAndLoadAsync();
        }
    }

    private string _searchQuery = string.Empty;
    public string SearchQuery
    {
        get => _searchQuery;
        set => SetField(ref _searchQuery, value);
    }

    private bool _noVideo = true;
    public bool NoVideo
    {
        get => _noVideo;
        set => SetField(ref _noVideo, value);
    }

    private bool _autoInstall = true;
    public bool AutoInstall
    {
        get => _autoInstall;
        set => SetField(ref _autoInstall, value);
    }

    private bool _showDownloaded;
    public bool ShowDownloaded
    {
        get => _showDownloaded;
        set
        {
            if (SetField(ref _showDownloaded, value))
                _ = ResetAndLoadAsync();
        }
    }

    private bool _allDownloadedHint;
    public bool AllDownloadedHint
    {
        get => _allDownloadedHint;
        set => SetField(ref _allDownloadedHint, value);
    }

    public bool IsLoading
    {
        get => _isLoading;
        set => SetField(ref _isLoading, value);
    }

    private string _loadingText = "Loading...";
    public string LoadingText
    {
        get => _loadingText;
        set => SetField(ref _loadingText, value);
    }

    public bool IsSupporter { get; }

    public DownloadManager GetDownloadManager() => _downloadManager;
    public void SaveState() => SaveDiskCache();

    public ICommand DownloadCommand { get; }
    public ICommand PlayPreviewCommand { get; }
    public ICommand SearchCommand { get; }
    public ICommand RetryCommand { get; }
    public ICommand CancelCommand { get; }
    public ICommand CancelAllCommand { get; }
    public ICommand DownloadAllCommand { get; }

    public MainViewModel(AppSettings settings)
    {
        _settings = settings;
        _api = new OsuApiService(settings);
        IsSupporter = settings.IsSupporter;

        var songsPath = Services.DataPaths.TempSongsDir;
        var osuSongsPath = string.IsNullOrWhiteSpace(settings.OsuSongsPath) ? null : settings.OsuSongsPath;
        _downloadManager = new DownloadManager(songsPath, osuSongsPath, OnDownloadCompleted, OnDownloadFailed, IsSupporter);

        DownloadCommand = new RelayCommand(o => ExecuteDownload(o as BeatmapSet));
        PlayPreviewCommand = new RelayCommand(o => ExecutePlayPreview(o as BeatmapSet));
        SearchCommand = new RelayCommand(_ => _ = ResetAndLoadAsync());
        RetryCommand = new RelayCommand(o => ExecuteRetry(o as DownloadItem));
        CancelCommand = new RelayCommand(o => ExecuteCancel(o as DownloadItem));
        CancelAllCommand = new RelayCommand(_ => ExecuteCancelAll());
        DownloadAllCommand = new RelayCommand(_ => ExecuteDownloadAll());

        LoadDownloadedIds();
        LoadDiskCache();
    }

    private void LoadDownloadedIds()
    {
        if (Directory.Exists(_settings.OsuSongsPath))
        {
            foreach (var dir in Directory.GetDirectories(_settings.OsuSongsPath))
            {
                var folderName = Path.GetFileName(dir);
                var spaceIndex = folderName.IndexOf(' ');
                if (spaceIndex > 0 && int.TryParse(folderName[..spaceIndex], out var id))
                {
                    // Only count as downloaded if folder has actual content
                    if (Directory.GetFiles(dir).Length > 0)
                        _downloadedIds.Add(id);
                }
            }
        }

        var localSongsPath = Services.DataPaths.TempSongsDir;
        if (Directory.Exists(localSongsPath))
        {
            foreach (var file in Directory.GetFiles(localSongsPath, "*.osz"))
            {
                var fileName = Path.GetFileNameWithoutExtension(file);
                var spaceIndex = fileName.IndexOf(' ');
                if (spaceIndex > 0 && int.TryParse(fileName[..spaceIndex], out var id))
                    _downloadedIds.Add(id);
            }
        }
    }

    public async Task InitializeAsync()
    {
        if (!await _api.AuthenticateAsync())
        {
            MessageBox.Show("Failed to authenticate with osu! API. Check your Client ID and Secret.",
                "Authentication Error", MessageBoxButton.OK, MessageBoxImage.Error);
            return;
        }

        // Resume any pending downloads from last session
        _downloadManager.LoadAndResumeQueue();

        await ResetAndLoadAsync();
    }

    private async Task ResetAndLoadAsync()
    {
        // Cancel previous load
        _loadCts.Cancel();
        _loadCts = new CancellationTokenSource();
        var ct = _loadCts.Token;

        // Parse query: extract filter expressions, keep text query
        var parsed = SearchQueryParser.Parse(SearchQuery);
        _activeFilter = parsed;

        // Snapshot current filters
        _activeMode = SelectedMode;
        _activeStatus = SelectedStatus;
        _activeQuery = string.IsNullOrWhiteSpace(parsed.TextQuery) ? null : parsed.TextQuery;

        Log($"[RESET] mode={_activeMode} status={_activeStatus} query={_activeQuery ?? "null"} filters={_activeFilter.Filters.Count}");

        IsLoading = false;
        AllDownloadedHint = false;
        Beatmaps.Clear();

        // Try to restore from cache
        var showDownloaded = ShowDownloaded;
        var filter = _activeFilter;
        var cache = GetOrCreateCache();

        // Discard stale empty cache (failed or cancelled loads)
        if (cache.AllResults.Count == 0 && !cache.HasMore)
        {
            _cache.Remove(CacheKey);
            cache = GetOrCreateCache();
        }

        if (cache.AllResults.Count > 0)
        {
            foreach (var beatmap in cache.AllResults)
            {
                var isDownloaded = _downloadedIds.Contains(beatmap.Id);
                if (isDownloaded && !showDownloaded)
                    continue;
                if (!filter.Matches(beatmap))
                    continue;
                beatmap.IsDownloaded = isDownloaded;
                Beatmaps.Add(beatmap);
            }

            if (!cache.HasMore)
                return;
        }

        Log($"[RESET] cache={cache.AllResults.Count} hasMore={cache.HasMore} visible={Beatmaps.Count} calling LoadMoreAsync...");
        await LoadMoreAsync(ct, force: true);
        Log($"[RESET] after LoadMore: visible={Beatmaps.Count}");
    }

    public async Task LoadMoreAsync()
    {
        await LoadMoreAsync(_loadCts.Token, force: false);
    }

    private async Task LoadMoreAsync(CancellationToken ct, bool force = false)
    {
        var cache = GetOrCreateCache();

        if ((!force && IsLoading) || !cache.HasMore || ct.IsCancellationRequested)
        {
            Log($"[LOAD] SKIPPED: IsLoading={IsLoading} force={force} HasMore={cache.HasMore} Cancelled={ct.IsCancellationRequested}");
            return;
        }
        IsLoading = true;
        LoadingText = "Loading...";

        var mode = _activeMode;
        var status = _activeStatus;
        var query = _activeQuery;

        try
        {
            var showDl = ShowDownloaded;
            var filter = _activeFilter;
            var visibleBefore = Beatmaps.Count;
            var loadStart = DateTime.UtcNow;

            // Keep fetching until we have new visible results or API is exhausted
            while (cache.HasMore)
            {
                if ((DateTime.UtcNow - loadStart).TotalSeconds >= 3 && Beatmaps.Count == visibleBefore)
                    LoadingText = "Loading... (takes longer, seems you already have a lot of maps)";
                if (ct.IsCancellationRequested) return;

                var result = await _api.SearchBeatmapsAsync(
                    query: query,
                    mode: mode,
                    status: status,
                    cursorString: cache.CursorString);

                if (ct.IsCancellationRequested) return;

                if (result == null || ct.IsCancellationRequested)
                {
                    if (!ct.IsCancellationRequested)
                        cache.HasMore = false;
                    return;
                }

                cache.CursorString = result.CursorString;
                cache.HasMore = result.CursorString != null;

                foreach (var beatmap in result.BeatmapSets)
                {
                    cache.AllResults.Add(beatmap);

                    var isDownloaded = _downloadedIds.Contains(beatmap.Id);
                    if (!filter.Matches(beatmap))
                        continue;
                    if (isDownloaded && !showDl)
                        continue;

                    beatmap.IsDownloaded = isDownloaded;
                    Beatmaps.Add(beatmap);
                }

                // Stop once we have some new visible results
                if (Beatmaps.Count > visibleBefore)
                    break;
            }

            // If we exhausted all pages and still nothing visible
            if (Beatmaps.Count == 0 && cache.AllResults.Count > 0 && !cache.HasMore && !ct.IsCancellationRequested)
            {
                AllDownloadedHint = true;
            }
        }
        finally
        {
            if (!ct.IsCancellationRequested)
                IsLoading = false;
        }
    }

    private void ExecuteDownloadAll()
    {
        if (!IsSupporter) return;

        var toDownload = Beatmaps.Where(b => !b.IsQueued && !b.IsDownloaded).Take(100).ToList();
        foreach (var beatmap in toDownload)
        {
            beatmap.IsQueued = true;
            _downloadManager.Enqueue(beatmap, NoVideo, AutoInstall);
        }
    }

    private void ExecuteDownload(BeatmapSet? beatmap)
    {
        if (beatmap == null || beatmap.IsQueued) return;
        if (!_downloadManager.CanDownload())
        {
            _downloadManager.Enqueue(beatmap, NoVideo, AutoInstall); // triggers rate limit message
            return;
        }
        beatmap.IsQueued = true;
        _downloadManager.Enqueue(beatmap, NoVideo, AutoInstall);
    }

    private void ExecutePlayPreview(BeatmapSet? beatmap)
    {
        if (beatmap == null) return;
        if (!IsSupporter) return;

        // If downloaded, try to play from local osu! Songs folder
        if (beatmap.IsDownloaded && !string.IsNullOrWhiteSpace(_settings.OsuSongsPath))
        {
            var localFile = FindLocalAudio(beatmap.Id);
            if (localFile != null)
            {
                _audio.PlayLocal(beatmap.Id, localFile);
                return;
            }
        }

        _audio.PlayPreview(beatmap.Id);
    }

    private string? FindLocalAudio(int beatmapSetId)
    {
        if (!Directory.Exists(_settings.OsuSongsPath)) return null;

        // Find the beatmap folder (starts with the set ID)
        var folder = Directory.GetDirectories(_settings.OsuSongsPath)
            .FirstOrDefault(d => Path.GetFileName(d).StartsWith($"{beatmapSetId} "));
        if (folder == null) return null;

        // Find audio file — look for the one referenced in .osu files, or just pick the first audio file
        var audioFile = Directory.GetFiles(folder, "*.mp3").FirstOrDefault()
                     ?? Directory.GetFiles(folder, "*.ogg").FirstOrDefault();
        return audioFile;
    }

    private void ExecuteRetry(DownloadItem? item)
    {
        if (item == null) return;
        _downloadManager.Retry(item, NoVideo, AutoInstall);
    }

    private void ExecuteCancel(DownloadItem? item)
    {
        if (item == null) return;
        _downloadManager.Cancel(item);
        // Re-show download button in beatmap list
        var beatmap = Beatmaps.FirstOrDefault(b => b.Id == item.BeatmapSetId);
        if (beatmap != null) beatmap.IsQueued = false;
    }

    private void ExecuteCancelAll()
    {
        // Collect IDs before clearing
        var ids = _downloadManager.AllItems.Select(i => i.BeatmapSetId).ToHashSet();
        _downloadManager.CancelAll();
        // Re-show download buttons
        foreach (var beatmap in Beatmaps)
        {
            if (ids.Contains(beatmap.Id))
                beatmap.IsQueued = false;
        }
    }

    private void OnDownloadCompleted(int beatmapSetId)
    {
        _downloadedIds.Add(beatmapSetId);
        AudioService.RemoveCache(beatmapSetId);

        Application.Current.Dispatcher.Invoke(() =>
        {
            var toRemove = Beatmaps.FirstOrDefault(b => b.Id == beatmapSetId);
            if (toRemove != null)
                Beatmaps.Remove(toRemove);
        });
    }

    private void OnDownloadFailed(int beatmapSetId)
    {
        Application.Current.Dispatcher.BeginInvoke(() =>
        {
            var beatmap = Beatmaps.FirstOrDefault(b => b.Id == beatmapSetId);
            if (beatmap != null)
                beatmap.IsQueued = false;
        });
    }
}
