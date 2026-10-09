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
        var v = typeof(AboutPage).Assembly.GetName().Version;
        Version.Text = $"Version {v?.Major}.{v?.Minor}.{v?.Build}";
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
}
