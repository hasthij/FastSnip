using FastSnip.App.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FastSnip.App.Pages;

public sealed partial class AboutPage : Page
{
    public AboutPage()
    {
        InitializeComponent();
        NavigationCacheMode = Microsoft.UI.Xaml.Navigation.NavigationCacheMode.Required;
        Version.Text = $"Version {AppVersion()}";
    }

    /// The installed package's version (what Windows shows), or the build's when not packaged.
    public static string AppVersion()
    {
        try
        {
            var v = Windows.ApplicationModel.Package.Current.Id.Version;
            return $"{v.Major}.{v.Minor}.{v.Build}";
        }
        catch
        {
            var v = typeof(AboutPage).Assembly.GetName().Version;
            return $"{v?.Major}.{v?.Minor}.{v?.Build} (development)";
        }
    }

    private async void OpenShots_Click(object sender, RoutedEventArgs e)
    {
        var dir = Captures.ScreenshotsDir(App.Settings);
        Directory.CreateDirectory(dir);
        await Windows.System.Launcher.LaunchFolderPathAsync(dir);
    }

    private async void OpenConfig_Click(object sender, RoutedEventArgs e)
    {
        Directory.CreateDirectory(Config.Dir);
        await Windows.System.Launcher.LaunchFolderPathAsync(Config.RealDir);
    }

    private void Setup_Click(object sender, RoutedEventArgs e) => App.Window?.Show("setup");

    private void Quit_Click(object sender, RoutedEventArgs e)
    {
        // The core stops itself and its listener, and closes this window too.
        Core.Run("--quit");
        App.Window?.Close();
    }
}
