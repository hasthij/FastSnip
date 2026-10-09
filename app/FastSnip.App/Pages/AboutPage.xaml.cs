using FastSnip.App.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FastSnip.App.Pages;

public sealed partial class AboutPage : Page
{
    public AboutPage()
    {
        InitializeComponent();
        var v = typeof(AboutPage).Assembly.GetName().Version;
        Version.Text = $"Version {v?.Major}.{v?.Minor}.{v?.Build}";
    }

    private void OpenConfig_Click(object sender, RoutedEventArgs e)
    {
        Directory.CreateDirectory(Config.Dir);
        System.Diagnostics.Process.Start("explorer.exe", Config.Dir);
    }

    private void Setup_Click(object sender, RoutedEventArgs e) => App.Window?.Show("setup");
}
