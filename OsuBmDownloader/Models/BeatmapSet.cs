using System.ComponentModel;
using System.Runtime.CompilerServices;
using Newtonsoft.Json;

namespace OsuBmDownloader.Models;

public class BeatmapSet : INotifyPropertyChanged
{
    [JsonProperty("id")]
    public int Id { get; set; }

    [JsonProperty("title")]
    public string Title { get; set; } = string.Empty;

    [JsonProperty("artist")]
    public string Artist { get; set; } = string.Empty;

    [JsonProperty("creator")]
    public string Creator { get; set; }= string.Empty;

    [JsonProperty("status")]
    public string Status { get; set; } = string.Empty;

    [JsonProperty("ranked_date")]
    public DateTime? RankedDate { get; set; }

    [JsonProperty("submitted_date")]
    public DateTime? SubmittedDate { get; set; }

    public string DateText
    {
        get
        {
            var date = RankedDate ?? SubmittedDate;
            return date.HasValue ? date.Value.ToString("yyyy-MM-dd") : "";
        }
    }

    [JsonProperty("covers")]
    public BeatmapCovers Covers { get; set; } = new();

    [JsonProperty("beatmaps")]
    public List<Beatmap> Beatmaps { get; set; } = new();

    public double MinStarRating => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.DifficultyRating) : 0;
    public double MaxStarRating => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.DifficultyRating) : 0;
    public double MinBpm => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.Bpm) : 0;
    public double MaxBpm => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.Bpm) : 0;
    public int MinLength => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.TotalLength) : 0;
    public int MaxLength => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.TotalLength) : 0;
    public double MinAR => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.AR) : 0;
    public double MaxAR => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.AR) : 0;
    public double MinCS => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.CS) : 0;
    public double MaxCS => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.CS) : 0;
    public double MinOD => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.OD) : 0;
    public double MaxOD => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.OD) : 0;
    public double MinHP => Beatmaps.Count > 0 ? Beatmaps.Min(b => b.HP) : 0;
    public double MaxHP => Beatmaps.Count > 0 ? Beatmaps.Max(b => b.HP) : 0;

    public string StarRangeText => Beatmaps.Count > 0
        ? $"★ {MinStarRating:F1} - {MaxStarRating:F1}"
        : "★ ?";

    public string PreviewUrl => $"https://b.ppy.sh/preview/{Id}.mp3";
    public string CoverUrl => Covers.List2x ?? Covers.List ?? Covers.Cover ?? string.Empty;

    private bool _isQueued;
    [JsonIgnore]
    public bool IsQueued
    {
        get => _isQueued;
        set { _isQueued = value; OnPropertyChanged(); OnPropertyChanged(nameof(ShowDownloadButton)); }
    }

    private bool _isDownloaded;
    [JsonIgnore]
    public bool IsDownloaded
    {
        get => _isDownloaded;
        set { _isDownloaded = value; OnPropertyChanged(); OnPropertyChanged(nameof(ShowDownloadButton)); }
    }

    [JsonIgnore]
    public bool ShowDownloadButton => !IsQueued && !IsDownloaded;

    public event PropertyChangedEventHandler? PropertyChanged;
    protected void OnPropertyChanged([CallerMemberName] string? name = null)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}

public class BeatmapCovers
{
    [JsonProperty("cover")]
    public string? Cover { get; set; }

    [JsonProperty("cover@2x")]
    public string? Cover2x { get; set; }

    [JsonProperty("list")]
    public string? List { get; set; }

    [JsonProperty("list@2x")]
    public string? List2x { get; set; }
}

public class Beatmap
{
    [JsonProperty("id")]
    public int Id { get; set; }

    [JsonProperty("difficulty_rating")]
    public double DifficultyRating { get; set; }

    [JsonProperty("mode")]
    public string Mode { get; set; } = string.Empty;

    [JsonProperty("bpm")]
    public double Bpm { get; set; }

    [JsonProperty("total_length")]
    public int TotalLength { get; set; }

    [JsonProperty("ar")]
    public double AR { get; set; }

    [JsonProperty("cs")]
    public double CS { get; set; }

    [JsonProperty("accuracy")] // OD is mapped as "accuracy" in the API
    public double OD { get; set; }

    [JsonProperty("drain")] // HP is mapped as "drain" in the API
    public double HP { get; set; }
}

public class BeatmapSearchResponse
{
    [JsonProperty("beatmapsets")]
    public List<BeatmapSet> BeatmapSets { get; set; } = new();

    [JsonProperty("cursor_string")]
    public string? CursorString { get; set; }
}

public class OAuthTokenResponse
{
    [JsonProperty("access_token")]
    public string AccessToken { get; set; } = string.Empty;

    [JsonProperty("expires_in")]
    public int ExpiresIn { get; set; }

    [JsonProperty("token_type")]
    public string TokenType { get; set; } = string.Empty;
}

public class OsuUser
{
    [JsonProperty("id")]
    public int Id { get; set; }

    [JsonProperty("username")]
    public string Username { get; set; } = string.Empty;

    [JsonProperty("avatar_url")]
    public string AvatarUrl { get; set; } = string.Empty;

    [JsonProperty("is_supporter")]
    public bool IsSupporter { get; set; }

    [JsonProperty("support_level")]
    public int SupportLevel { get; set; }
}
