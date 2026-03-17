using System.Diagnostics;
using System.Windows;
using System.Windows.Navigation;
using Microsoft.Win32;
using OsuBmDownloader.Models;

namespace OsuBmDownloader;

public partial class SettingsDialog : Window
{
    public AppSettings Settings { get; }

    public SettingsDialog(AppSettings settings)
    {
        InitializeComponent();
        Settings = settings;
        ClientIdBox.Text = settings.ClientId;
        ClientSecretBox.Password = settings.ClientSecret;
        OsuFolderBox.Text = settings.OsuPath;
    }

    private void BrowseFolder_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFolderDialog
        {
            Title = "Select osu! Songs folder",
            InitialDirectory = string.IsNullOrEmpty(OsuFolderBox.Text) ? null : OsuFolderBox.Text
        };

        if (dialog.ShowDialog() == true)
            OsuFolderBox.Text = dialog.FolderName;
    }

    private void Hyperlink_Navigate(object sender, RequestNavigateEventArgs e)
    {
        Process.Start(new ProcessStartInfo(e.Uri.AbsoluteUri) { UseShellExecute = true });
        e.Handled = true;
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrWhiteSpace(ClientIdBox.Text) || string.IsNullOrWhiteSpace(ClientSecretBox.Password))
        {
            MessageBox.Show("Please enter both Client ID and Client Secret.", "Missing Fields",
                MessageBoxButton.OK, MessageBoxImage.Warning);
            return;
        }

        Settings.ClientId = ClientIdBox.Text.Trim();
        Settings.ClientSecret = ClientSecretBox.Password.Trim();
        Settings.OsuPath = OsuFolderBox.Text.Trim();
        Settings.Save();

        DialogResult = true;
        Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}
