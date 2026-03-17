using System.IO;
using System.Net.Http;
using NAudio.Wave;
using NAudio.Vorbis;

namespace OsuBmDownloader.Services;

public class AudioService
{
    private static readonly HttpClient _http = new();
    private static readonly string CacheDir = DataPaths.PreviewCacheDir;

    private IWavePlayer? _waveOut;
    private WaveStream? _reader;
    private int _currentBeatmapSetId;

    public void PlayPreview(int beatmapSetId)
    {
        if (_currentBeatmapSetId == beatmapSetId)
        {
            Stop();
            return;
        }

        Stop();
        _currentBeatmapSetId = beatmapSetId;
        _ = DownloadAndPlayAsync(beatmapSetId);
    }

    public void PlayLocal(int beatmapSetId, string filePath)
    {
        if (_currentBeatmapSetId == beatmapSetId)
        {
            Stop();
            return;
        }

        Stop();
        _currentBeatmapSetId = beatmapSetId;
        PlayFile(beatmapSetId, filePath);
    }

    private void PlayFile(int beatmapSetId, string filePath)
    {
        try
        {
            WaveStream reader = filePath.EndsWith(".ogg", StringComparison.OrdinalIgnoreCase)
                ? new VorbisWaveReader(filePath)
                : new Mp3FileReader(filePath);

            var waveOut = new WaveOutEvent();
            waveOut.Init(reader);
            waveOut.PlaybackStopped += (_, _) =>
            {
                if (_currentBeatmapSetId == beatmapSetId)
                    _currentBeatmapSetId = 0;
                reader.Dispose();
                waveOut.Dispose();
            };
            waveOut.Play();

            _waveOut = waveOut;
            _reader = reader;
        }
        catch
        {
            _currentBeatmapSetId = 0;
        }
    }

    private async Task DownloadAndPlayAsync(int beatmapSetId)
    {
        try
        {
            Directory.CreateDirectory(CacheDir);

            // Check cache first
            var cachedFile = Directory.GetFiles(CacheDir, $"{beatmapSetId}.*").FirstOrDefault();

            if (cachedFile == null)
            {
                var url = $"https://b.ppy.sh/preview/{beatmapSetId}.mp3";

                using var response = await _http.GetAsync(url, HttpCompletionOption.ResponseHeadersRead).ConfigureAwait(false);
                response.EnsureSuccessStatusCode();

                var contentType = response.Content.Headers.ContentType?.MediaType ?? "";
                var ext = contentType.Contains("ogg") ? ".ogg" : ".mp3";

                var data = await response.Content.ReadAsByteArrayAsync().ConfigureAwait(false);

                if (_currentBeatmapSetId != beatmapSetId) return;

                cachedFile = Path.Combine(CacheDir, $"{beatmapSetId}{ext}");
                await File.WriteAllBytesAsync(cachedFile, data).ConfigureAwait(false);
            }

            if (_currentBeatmapSetId != beatmapSetId) return;

            await System.Windows.Application.Current.Dispatcher.InvokeAsync(() =>
            {
                if (_currentBeatmapSetId != beatmapSetId) return;

                WaveStream reader = cachedFile.EndsWith(".ogg")
                    ? new VorbisWaveReader(cachedFile)
                    : new Mp3FileReader(cachedFile);

                var waveOut = new WaveOutEvent();
                waveOut.Init(reader);
                waveOut.PlaybackStopped += (_, _) =>
                {
                    // Only clean up if this is still the active player
                    if (_currentBeatmapSetId == beatmapSetId)
                        _currentBeatmapSetId = 0;
                    reader.Dispose();
                    waveOut.Dispose();
                };
                waveOut.Play();

                _waveOut = waveOut;
                _reader = reader;
            });
        }
        catch
        {
            if (_currentBeatmapSetId == beatmapSetId)
                _currentBeatmapSetId = 0;
        }
    }

    public void Stop()
    {
        var waveOut = _waveOut;
        var reader = _reader;
        _waveOut = null;
        _reader = null;
        _currentBeatmapSetId = 0;

        waveOut?.Stop();
        reader?.Dispose();
        waveOut?.Dispose();
    }

    /// <summary>
    /// Remove cached preview for a beatmap set (call after download completes).
    /// </summary>
    public static void RemoveCache(int beatmapSetId)
    {
        try
        {
            if (!Directory.Exists(CacheDir)) return;
            foreach (var file in Directory.GetFiles(CacheDir, $"{beatmapSetId}.*"))
                File.Delete(file);
        }
        catch { }
    }

    public bool IsPlaying(int beatmapSetId) => _currentBeatmapSetId == beatmapSetId;
}
