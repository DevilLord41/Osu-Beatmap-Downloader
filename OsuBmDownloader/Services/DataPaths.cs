using System.IO;

namespace OsuBmDownloader.Services;

public static class DataPaths
{
    public static readonly string DataDir = Path.Combine(
        AppDomain.CurrentDomain.BaseDirectory, "data");

    public static readonly string SettingsFile = Path.Combine(DataDir, "settings.dat");
    public static readonly string RateLimitFile = Path.Combine(DataDir, "rate_limit.dat");
    public static readonly string DownloadQueueFile = Path.Combine(DataDir, "download_queue.dat");
    public static readonly string SearchCacheFile = Path.Combine(DataDir, "search_cache.dat");
    public static readonly string DebugLogFile = Path.Combine(DataDir, "debug.log");
    public static readonly string TempSongsDir = Path.Combine(DataDir, "_temp_songs");
    public static readonly string PreviewCacheDir = Path.Combine(DataDir, "_preview_cache");

    static DataPaths()
    {
        Directory.CreateDirectory(DataDir);
    }
}
