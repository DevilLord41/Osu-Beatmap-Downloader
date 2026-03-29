using System.Configuration;
using System.Data;
using System.Windows;

namespace OsuBmDownloader;

/// <summary>
/// Interaction logic for App.xaml
/// </summary>
public partial class App : Application
{
    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        DispatcherUnhandledException += (_, args) =>
        {
            var logPath = Services.DataPaths.DebugLogFile;
            System.IO.File.AppendAllText(logPath,
                $"{DateTime.Now:HH:mm:ss.fff} [CRASH] {args.Exception}\n");
            MessageBox.Show(args.Exception.ToString(), "Error", MessageBoxButton.OK, MessageBoxImage.Error);
            args.Handled = true;
        };
    }
}

