using System.Globalization;
using System.Windows.Data;
using System.Windows.Media;

namespace OsuBmDownloader.Converters;

public class StatusToColorConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        var status = value?.ToString()?.ToLower() ?? "";
        return status switch
        {
            "ranked" => new SolidColorBrush(Color.FromRgb(104, 195, 163)),
            "qualified" => new SolidColorBrush(Color.FromRgb(190, 217, 126)),
            "loved" => new SolidColorBrush(Color.FromRgb(255, 102, 171)),
            "pending" => new SolidColorBrush(Color.FromRgb(230, 200, 100)),
            "graveyard" => new SolidColorBrush(Color.FromRgb(128, 128, 128)),
            _ => new SolidColorBrush(Color.FromRgb(170, 170, 170))
        };
    }

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

public class DownloadStatusToColorConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        if (value is Models.DownloadStatus status)
        {
            return status switch
            {
                Models.DownloadStatus.Downloading => new SolidColorBrush(Color.FromRgb(102, 204, 255)),
                Models.DownloadStatus.Extracting => new SolidColorBrush(Color.FromRgb(255, 204, 102)),
                Models.DownloadStatus.Queued => new SolidColorBrush(Color.FromRgb(170, 170, 170)),
                Models.DownloadStatus.Completed => new SolidColorBrush(Color.FromRgb(104, 195, 163)),
                Models.DownloadStatus.Failed => new SolidColorBrush(Color.FromRgb(255, 102, 102)),
                _ => new SolidColorBrush(Colors.White)
            };
        }
        return new SolidColorBrush(Colors.White);
    }

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

public class ProgressToWidthConverter : IMultiValueConverter
{
    public object Convert(object[] values, Type targetType, object parameter, CultureInfo culture)
    {
        if (values.Length == 2 && values[0] is double progress && values[1] is double actualWidth)
            return actualWidth * (progress / 100.0);
        return 0.0;
    }

    public object[] ConvertBack(object value, Type[] targetTypes, object parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

public class BoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
        => value is true ? System.Windows.Visibility.Visible : System.Windows.Visibility.Collapsed;

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

public class InverseBoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
        => value is true ? System.Windows.Visibility.Collapsed : System.Windows.Visibility.Visible;

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture)
        => throw new NotSupportedException();
}

public class DownloadStatusToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture)
    {
        if (value is Models.DownloadStatus status && parameter is string target)
        {
            return status.ToString().ToLower() == target.ToLower()
                ? System.Windows.Visibility.Visible
                : System.Windows.Visibility.Collapsed;
        }
        return System.Windows.Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture)
        => throw new NotSupportedException();
}
