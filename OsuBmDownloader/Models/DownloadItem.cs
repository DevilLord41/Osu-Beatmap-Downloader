using System.ComponentModel;
using System.Runtime.CompilerServices;

namespace OsuBmDownloader.Models;

public enum DownloadStatus
{
    Queued,
    Downloading,
    Extracting,
    Completed,
    Failed
}

public class DownloadItem : INotifyPropertyChanged
{
    public int BeatmapSetId { get; set; }
    public string Title { get; set; } = string.Empty;
    public string Artist { get; set; } = string.Empty;
    public CancellationTokenSource Cts { get; } = new();

    private double _progress;
    public double Progress
    {
        get => _progress;
        set { _progress = value; OnPropertyChanged(); OnPropertyChanged(nameof(ProgressText)); }
    }

    private DownloadStatus _status = DownloadStatus.Queued;
    public DownloadStatus Status
    {
        get => _status;
        set { _status = value; OnPropertyChanged(); OnPropertyChanged(nameof(StatusText)); }
    }

    public string ProgressText => Status == DownloadStatus.Downloading
        ? $"{Progress:F0}%"
        : StatusText;

    public string StatusText => Status switch
    {
        DownloadStatus.Queued => "queued",
        DownloadStatus.Downloading => $"{Progress:F0}%",
        DownloadStatus.Extracting => "extracting...",
        DownloadStatus.Completed => "done",
        DownloadStatus.Failed => "failed",
        _ => ""
    };

    public string DisplayName => $"{Artist} - {Title}";

    public event PropertyChangedEventHandler? PropertyChanged;
    protected void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
