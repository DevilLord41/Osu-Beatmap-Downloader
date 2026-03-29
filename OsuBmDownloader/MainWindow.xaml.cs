using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;
using OsuBmDownloader.Models;
using OsuBmDownloader.Services;
using OsuBmDownloader.ViewModels;

namespace OsuBmDownloader;

public partial class MainWindow : Window
{
    private MainViewModel? _viewModel;
    private AppSettings _currentSettings = null!;
    private DispatcherTimer? _rateLimitTimer;
    private DispatcherTimer? _searchDebounce;

    public MainWindow()
    {
        InitializeComponent();
        Loaded += MainWindow_Loaded;
        Closing += MainWindow_Closing;
    }

    private void MainWindow_Closing(object? sender, System.ComponentModel.CancelEventArgs e)
    {
        if (_viewModel == null) return;
        _viewModel.GetDownloadManager().SaveQueue(_viewModel.NoVideo, _viewModel.AutoInstall);
        _viewModel.SaveState();
    }

    private async void MainWindow_Loaded(object sender, RoutedEventArgs e)
    {
        var settings = AppSettings.Load();

        if (!settings.IsConfigured)
        {
            var dialog = new SettingsDialog(settings);
            if (dialog.ShowDialog() != true)
            {
                Close();
                return;
            }
            settings = AppSettings.Load();
        }

        // Re-verify supporter status on every launch
        if (settings.IsLoggedIn && settings.IsSupporter)
        {
            await VerifySupporterStatus(settings);
        }

        _currentSettings = settings;
        ApplySettings(settings);
        await _viewModel!.InitializeAsync();
    }

    private async Task VerifySupporterStatus(AppSettings settings)
    {
        try
        {
            var api = new OsuApiService(settings);
            if (!api.TryRestoreUserToken())
                return; // Token expired or missing — skip silent check, don't open browser

            var user = await api.GetMeAsync();
            if (user != null)
            {
                var wasSupporter = settings.IsSupporter;
                settings.IsSupporter = user.IsSupporter;
                settings.SupportLevel = user.SupportLevel;
                settings.Username = user.Username;
                settings.Save();

                if (wasSupporter && !user.IsSupporter)
                {
                    MessageBox.Show(
                        "Your osu! Supporter status has expired.\nPremium features have been disabled.",
                        "Supporter Expired", MessageBoxButton.OK, MessageBoxImage.Information);
                }
            }
        }
        catch
        {
            // Network error — keep previous state, don't block the app
        }
    }

    private void ApplySettings(AppSettings settings)
    {
        _viewModel = new MainViewModel(settings);
        DataContext = _viewModel;
        UpdateSupporterUI(settings);
        StartRateLimitTimer(settings);
    }

    private void StartRateLimitTimer(AppSettings settings)
    {
        _rateLimitTimer?.Stop();

        if (settings.IsSupporter)
        {
            RateLimitText.Text = "";
            return;
        }

        UpdateRateLimitDisplay();

        _rateLimitTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        _rateLimitTimer.Tick += (_, _) => UpdateRateLimitDisplay();
        _rateLimitTimer.Start();
    }

    private void UpdateRateLimitDisplay()
    {
        if (_viewModel == null) return;
        var dm = _viewModel.GetDownloadManager();
        var text = dm.RateLimitText;
        var cooldown = dm.CooldownSeconds;

        RateLimitText.Text = text;
        RateLimitText.Foreground = cooldown > 0
            ? new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(255, 150, 100))
            : new System.Windows.Media.SolidColorBrush(System.Windows.Media.Color.FromRgb(136, 204, 136));
    }

    private void UpdateSupporterUI(AppSettings settings)
    {
        if (settings.IsSupporter)
        {
            SupporterButton.Visibility = Visibility.Collapsed;
            SupporterBadge.Visibility = Visibility.Visible;
            DownloadAllButton.Visibility = Visibility.Visible;

            // Show hearts based on support level (1-3)
            var hearts = Math.Max(1, Math.Min(3, settings.SupportLevel));
            SupporterHearts.Text = new string('♥', hearts);

            UserStatusText.Content = settings.Username;
            UserStatusText.Visibility = Visibility.Visible;
            UserStatusText.Foreground = new System.Windows.Media.SolidColorBrush(
                System.Windows.Media.Color.FromRgb(255, 102, 171));
        }
        else
        {
            SupporterButton.Visibility = Visibility.Visible;
            SupporterBadge.Visibility = Visibility.Collapsed;
            DownloadAllButton.Visibility = Visibility.Collapsed;
            UserStatusText.Content = settings.IsLoggedIn ? settings.Username : "";
            UserStatusText.Visibility = settings.IsLoggedIn ? Visibility.Visible : Visibility.Collapsed;
            UserStatusText.Foreground = new System.Windows.Media.SolidColorBrush(
                System.Windows.Media.Color.FromRgb(136, 136, 136));
        }
    }

    private void Logout_Click(object sender, RoutedEventArgs e)
    {
        var result = MessageBox.Show(
            $"Logged in as {_currentSettings.Username}.\n\nDo you want to logout?",
            "Logout", MessageBoxButton.YesNo, MessageBoxImage.Question);

        if (result != MessageBoxResult.Yes) return;

        _currentSettings.Username = string.Empty;
        _currentSettings.IsSupporter = false;
        _currentSettings.IsLoggedIn = false;
        _currentSettings.UserAccessToken = null;
        _currentSettings.UserTokenExpiry = DateTime.MinValue;
        _currentSettings.Save();

        ApplySettings(_currentSettings);
        _ = _viewModel!.InitializeAsync();
    }

    private async void Supporter_Click(object sender, RoutedEventArgs e)
    {
        var settings = _currentSettings;

        SupporterButton.IsEnabled = false;
        SupporterButton.Content = "Waiting for login...";

        try
        {
            var api = new OsuApiService(settings);

            if (await api.AuthenticateUserAsync())
            {
                var user = await api.GetMeAsync();
                if (user != null)
                {
                    settings.Username = user.Username;
                    settings.IsLoggedIn = true;

                    if (user.IsSupporter)
                    {
                        settings.IsSupporter = true;
                        settings.SupportLevel = user.SupportLevel;
                        settings.Save();
                        _currentSettings = settings;

                        ApplySettings(settings);
                        await _viewModel!.InitializeAsync();
                    }
                    else
                    {
                        settings.IsSupporter = false;
                        settings.Save();

                        MessageBox.Show(
                            $"Logged in as {user.Username}, but your account is not an osu! Supporter.\n\n" +
                            "Support osu! at osu.ppy.sh/home/support to unlock unlimited downloads and preview audio.",
                            "Not a Supporter", MessageBoxButton.OK, MessageBoxImage.Information);

                        UpdateSupporterUI(settings);
                    }
                }
                else
                {
                    MessageBox.Show("Login succeeded but failed to fetch user info.", "Error",
                        MessageBoxButton.OK, MessageBoxImage.Warning);
                }
            }
            else
            {
                MessageBox.Show("Login failed or timed out.\nMake sure your callback URL is set to:\nhttp://localhost:7270/callback",
                    "Login Failed", MessageBoxButton.OK, MessageBoxImage.Warning);
            }
        }
        catch (Exception ex)
        {
            MessageBox.Show($"Error: {ex.Message}", "Login Error",
                MessageBoxButton.OK, MessageBoxImage.Error);
        }
        finally
        {
            SupporterButton.IsEnabled = true;
            SupporterButton.Content = "I'm osu! Supporter";
        }
    }

    private void DownloadButton_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button) return;

        // Find the FlashOverlay in the same row
        var parent = VisualTreeHelper.GetParent(button);
        while (parent != null && parent is not Grid)
            parent = VisualTreeHelper.GetParent(parent);

        if (parent is Grid grid)
        {
            foreach (var child in grid.Children)
            {
                if (child is System.Windows.Controls.Border border && border.Name == "FlashOverlay")
                {
                    var anim = new DoubleAnimation
                    {
                        From = 0.5,
                        To = 0,
                        Duration = TimeSpan.FromSeconds(1),
                        EasingFunction = new QuadraticEase { EasingMode = EasingMode.EaseOut }
                    };
                    border.BeginAnimation(OpacityProperty, anim);
                    break;
                }
            }
        }
    }

    private void BeatmapInfo_Click(object sender, MouseButtonEventArgs e)
    {
        var logPath = Services.DataPaths.DebugLogFile;
        if (sender is FrameworkElement element && element.DataContext is BeatmapSet beatmap)
        {
            System.IO.File.AppendAllText(logPath, $"{DateTime.Now:HH:mm:ss.fff} [CLICK] id={beatmap.Id} title={beatmap.Title}\n");
            _viewModel?.PlayPreviewCommand.Execute(beatmap);
            e.Handled = true;
        }
        else
        {
            System.IO.File.AppendAllText(logPath, $"{DateTime.Now:HH:mm:ss.fff} [CLICK] sender={sender?.GetType().Name} dataContext={((sender as FrameworkElement)?.DataContext)?.GetType().Name ?? "null"}\n");
        }
    }

    private async void BeatmapList_ScrollChanged(object sender, ScrollChangedEventArgs e)
    {
        if (e.VerticalOffset + e.ViewportHeight >= e.ExtentHeight - 200)
        {
            if (_viewModel != null)
                await _viewModel.LoadMoreAsync();
        }
    }

    private string _lastSearchText = string.Empty;

    private void SearchBox_TextChanged(object sender, TextChangedEventArgs e)
    {
        // Update placeholder visibility
        SearchPlaceholder.Visibility = string.IsNullOrEmpty(SearchBox.Text)
            ? Visibility.Visible : Visibility.Collapsed;

        // Only debounce if text actually changed
        var currentText = SearchBox.Text ?? string.Empty;
        if (currentText == _lastSearchText) return;
        _lastSearchText = currentText;

        // Debounce: wait 400ms after user stops typing, then search
        _searchDebounce?.Stop();
        _searchDebounce = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(400) };
        _searchDebounce.Tick += (_, _) =>
        {
            _searchDebounce.Stop();
            _viewModel?.SearchCommand.Execute(null);
        };
        _searchDebounce.Start();
    }

    private ToggleButton[] _modeButtons = [];
    private bool _updatingMode;

    private void InitModeButtons()
    {
        _modeButtons = [ModeOsu, ModeTaiko, ModeCatch, ModeMania];
    }

    private void ModeButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel == null || sender is not ToggleButton clicked || _updatingMode) return;
        if (_modeButtons.Length == 0) InitModeButtons();

        _updatingMode = true;

        string newMode;

        if (clicked.IsChecked == true)
        {
            newMode = clicked.Tag?.ToString() ?? "all";
            foreach (var btn in _modeButtons)
            {
                if (btn != clicked)
                    btn.IsChecked = false;
            }
        }
        else
        {
            newMode = "all";
        }

        foreach (var btn in _modeButtons)
        {
            var img = btn.Content as System.Windows.Controls.Image;
            if (img != null) img.Opacity = btn.IsChecked == true ? 1.0 : 0.5;
        }

        _updatingMode = false;
        _viewModel.SelectedMode = newMode;
    }

    private void DownloadAll_Click(object sender, RoutedEventArgs e)
    {
        _viewModel?.DownloadAllCommand.Execute(null);
    }

    private void Settings_Click(object sender, RoutedEventArgs e)
    {
        var settings = AppSettings.Load();
        var dialog = new SettingsDialog(settings) { Owner = this };
        if (dialog.ShowDialog() == true)
        {
            settings = AppSettings.Load();
            _currentSettings = settings;
            ApplySettings(settings);
            _ = _viewModel!.InitializeAsync();
        }
    }
}
