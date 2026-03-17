using System.IO;
using Newtonsoft.Json;
using OsuBmDownloader.Services;

namespace OsuBmDownloader.Models;

public class AppSettings
{
    public string ClientId { get; set; } = string.Empty;
    public string ClientSecret { get; set; } = string.Empty;
    public string OsuPath { get; set; } = string.Empty;

    [JsonIgnore]
    public string OsuSongsPath => string.IsNullOrWhiteSpace(OsuPath) ? string.Empty : Path.Combine(OsuPath, "Songs");
    public bool PreferNoVideo { get; set; } = true;

    // User login state
    public string Username { get; set; } = string.Empty;
    public bool IsSupporter { get; set; }
    public int SupportLevel { get; set; }
    public bool IsLoggedIn { get; set; }

    // Saved user OAuth token for silent re-verification
    public string? UserAccessToken { get; set; }
    public DateTime UserTokenExpiry { get; set; } = DateTime.MinValue;

    private static readonly string SettingsPath = Services.DataPaths.SettingsFile;

    public static AppSettings Load()
    {
        var json = SecureStorage.ReadEncrypted(SettingsPath);
        if (json == null)
            return new AppSettings();

        return JsonConvert.DeserializeObject<AppSettings>(json) ?? new AppSettings();
    }

    public void Save()
    {
        var json = JsonConvert.SerializeObject(this);
        SecureStorage.WriteEncrypted(SettingsPath, json);
    }

    public bool IsConfigured => !string.IsNullOrWhiteSpace(ClientId) && !string.IsNullOrWhiteSpace(ClientSecret);
}
