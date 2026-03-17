using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using Newtonsoft.Json;
using OsuBmDownloader.Models;

namespace OsuBmDownloader.Services;

public class OsuApiService
{
    private const string BaseUrl = "https://osu.ppy.sh";
    private const string RedirectUri = "http://localhost:7270/callback";
    private readonly HttpClient _httpClient;
    private string? _accessToken;
    private DateTime _tokenExpiry = DateTime.MinValue;
    private readonly AppSettings _settings;
    private bool _isUserAuth;

    public bool IsUserAuthenticated => _isUserAuth && _accessToken != null && DateTime.UtcNow < _tokenExpiry;

    public OsuApiService(AppSettings settings)
    {
        _settings = settings;
        _httpClient = new HttpClient { Timeout = TimeSpan.FromSeconds(120) };
    }

    /// <summary>
    /// Authenticate with client credentials (public access only).
    /// </summary>
    public async Task<bool> AuthenticateAsync()
    {
        if (_accessToken != null && DateTime.UtcNow < _tokenExpiry)
            return true;

        var request = new HttpRequestMessage(HttpMethod.Post, $"{BaseUrl}/oauth/token")
        {
            Content = new FormUrlEncodedContent(new Dictionary<string, string>
            {
                ["client_id"] = _settings.ClientId,
                ["client_secret"] = _settings.ClientSecret,
                ["grant_type"] = "client_credentials",
                ["scope"] = "public"
            })
        };

        var response = await _httpClient.SendAsync(request);
        if (!response.IsSuccessStatusCode)
            return false;

        var json = await response.Content.ReadAsStringAsync();
        var token = JsonConvert.DeserializeObject<OAuthTokenResponse>(json);
        if (token == null)
            return false;

        _accessToken = token.AccessToken;
        _tokenExpiry = DateTime.UtcNow.AddSeconds(token.ExpiresIn - 60);
        _isUserAuth = false;
        return true;
    }

    /// <summary>
    /// Restore a previously saved user token without opening the browser.
    /// Returns true if the token is still valid.
    /// </summary>
    public bool TryRestoreUserToken()
    {
        if (!string.IsNullOrEmpty(_settings.UserAccessToken) && DateTime.UtcNow < _settings.UserTokenExpiry)
        {
            _accessToken = _settings.UserAccessToken;
            _tokenExpiry = _settings.UserTokenExpiry;
            _isUserAuth = true;
            return true;
        }
        return false;
    }

    /// <summary>
    /// Authenticate with Authorization Code flow (user-level access).
    /// Opens browser for login, listens on localhost for the callback.
    /// </summary>
    public async Task<bool> AuthenticateUserAsync()
    {
        var authorizeUrl = $"{BaseUrl}/oauth/authorize" +
            $"?client_id={_settings.ClientId}" +
            $"&redirect_uri={Uri.EscapeDataString(RedirectUri)}" +
            $"&response_type=code" +
            $"&scope=identify%20public";

        // Start local HTTP listener for the callback
        using var listener = new HttpListener();
        listener.Prefixes.Add("http://localhost:7270/");
        listener.Start();

        // Open browser
        Process.Start(new ProcessStartInfo(authorizeUrl) { UseShellExecute = true });

        // Wait for callback (with 30 second timeout)
        var contextTask = listener.GetContextAsync();
        var timeoutTask = Task.Delay(TimeSpan.FromSeconds(30));
        var completed = await Task.WhenAny(contextTask, timeoutTask);

        if (completed == timeoutTask)
        {
            listener.Stop();
            return false;
        }

        var context = await contextTask;
        var code = context.Request.QueryString["code"];
        var error = context.Request.QueryString["error"];
        var denied = !string.IsNullOrEmpty(error) || string.IsNullOrEmpty(code);

        // Send response to the browser
        var message = denied ? "Login cancelled. You can close this tab." : "Login successful! You can close this tab.";
        var responseHtml = "<html><body style='background:#1a1a2e;color:white;font-family:sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0'>"
            + $"<div style='text-align:center'><h1>{message}</h1></div></body></html>";
        var buffer = System.Text.Encoding.UTF8.GetBytes(responseHtml);
        context.Response.ContentType = "text/html";
        context.Response.ContentLength64 = buffer.Length;
        await context.Response.OutputStream.WriteAsync(buffer);
        context.Response.Close();
        listener.Stop();

        if (denied)
            return false;

        // Exchange code for token
        var tokenRequest = new HttpRequestMessage(HttpMethod.Post, $"{BaseUrl}/oauth/token")
        {
            Content = new FormUrlEncodedContent(new Dictionary<string, string>
            {
                ["client_id"] = _settings.ClientId,
                ["client_secret"] = _settings.ClientSecret,
                ["grant_type"] = "authorization_code",
                ["code"] = code,
                ["redirect_uri"] = RedirectUri
            })
        };

        var tokenResponse = await _httpClient.SendAsync(tokenRequest);
        if (!tokenResponse.IsSuccessStatusCode)
            return false;

        var json = await tokenResponse.Content.ReadAsStringAsync();
        var token = JsonConvert.DeserializeObject<OAuthTokenResponse>(json);
        if (token == null)
            return false;

        _accessToken = token.AccessToken;
        _tokenExpiry = DateTime.UtcNow.AddSeconds(token.ExpiresIn - 60);
        _isUserAuth = true;

        // Save token for silent re-use on next launch
        _settings.UserAccessToken = _accessToken;
        _settings.UserTokenExpiry = _tokenExpiry;
        _settings.Save();

        return true;
    }

    /// <summary>
    /// Get the authenticated user's info (requires user-level auth).
    /// </summary>
    public async Task<OsuUser?> GetMeAsync()
    {
        if (!IsUserAuthenticated)
            return null;

        var request = new HttpRequestMessage(HttpMethod.Get, $"{BaseUrl}/api/v2/me");
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _accessToken);

        var response = await _httpClient.SendAsync(request);
        if (!response.IsSuccessStatusCode)
            return null;

        var json = await response.Content.ReadAsStringAsync();
        return JsonConvert.DeserializeObject<OsuUser>(json);
    }

    public async Task<BeatmapSearchResponse?> SearchBeatmapsAsync(
        string? query = null,
        string? mode = null,
        string? status = null,
        string? cursorString = null)
    {
        if (!await AuthenticateAsync())
            return null;

        // Use appropriate sort: ranked_desc for ranked/qualified/loved, updated_desc for pending/graveyard
        var sort = status is "pending" or "graveyard" ? "updated_desc" : "ranked_desc";
        var parameters = new List<string> { $"sort={sort}" };

        if (!string.IsNullOrWhiteSpace(query))
            parameters.Add($"q={Uri.EscapeDataString(query)}");

        if (!string.IsNullOrWhiteSpace(mode) && mode != "all")
            parameters.Add($"m={GetModeIndex(mode)}");

        if (!string.IsNullOrWhiteSpace(status) && status != "any")
            parameters.Add($"s={status}");

        if (!string.IsNullOrWhiteSpace(cursorString))
            parameters.Add($"cursor_string={Uri.EscapeDataString(cursorString)}");

        var url = $"{BaseUrl}/api/v2/beatmapsets/search?{string.Join("&", parameters)}";

        var request = new HttpRequestMessage(HttpMethod.Get, url);
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", _accessToken);

        var response = await _httpClient.SendAsync(request);
        var logPath = DataPaths.DebugLogFile;
        if (!response.IsSuccessStatusCode)
        {
            System.IO.File.AppendAllText(logPath, $"{DateTime.Now:HH:mm:ss.fff} [API] FAILED {response.StatusCode}: {url}\n");
            return null;
        }

        var json = await response.Content.ReadAsStringAsync();
        var result = JsonConvert.DeserializeObject<BeatmapSearchResponse>(json);
        System.IO.File.AppendAllText(logPath, $"{DateTime.Now:HH:mm:ss.fff} [API] {url} -> {result?.BeatmapSets.Count ?? 0} results, cursor={result?.CursorString ?? "null"}\n");
        return result;
    }

    private static string GetModeIndex(string mode) => mode switch
    {
        "osu" => "0",
        "taiko" => "1",
        "catch" => "2",
        "mania" => "3",
        _ => ""
    };
}
