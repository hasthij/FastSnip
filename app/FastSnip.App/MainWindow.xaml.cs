using FastSnip.App.Pages;
using FastSnip.App.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using WinRT.Interop;

namespace FastSnip.App;

public sealed partial class MainWindow : Window
{
    public MainWindow()
    {
        InitializeComponent();
        ExtendsContentIntoTitleBar = true;
        SetTitleBar(TitleBar);
        Root.RequestedTheme = Look.Theme(App.Settings);
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));
        var hwnd = WindowNative.GetWindowHandle(this);
        var dpi = GetDpiForWindow(hwnd) / 96.0;
        AppWindow.Resize(new Windows.Graphics.SizeInt32((int)(1100 * dpi), (int)(760 * dpi)));
        if (AppWindow.Presenter is OverlappedPresenter p)
        {
            p.PreferredMinimumWidth = (int)(720 * dpi);
            p.PreferredMinimumHeight = (int)(520 * dpi);
        }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hwnd);

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(nint hwnd);

    public void ApplyTheme() => Root.RequestedTheme = Look.Theme(App.Settings);

    /// Re-read the accent and repaint every control with it (buttons, toggles, sliders).
    public void RefreshAccent()
    {
        Look.ApplyAccent(Application.Current.Resources, App.Settings);
        // Flipping the theme makes XAML look up every theme resource again.
        var want = Look.Theme(App.Settings);
        Root.RequestedTheme = Root.ActualTheme == ElementTheme.Dark ? ElementTheme.Light : ElementTheme.Dark;
        Root.RequestedTheme = want;
    }

    /// Show a page; `file` opens that capture in the editor.
    public void Show(string page, string? file = null)
    {
        if (page == "setup")
        {
            Nav.IsPaneVisible = false;
            ContentFrame.Navigate(typeof(SetupPage), null, new Microsoft.UI.Xaml.Media.Animation.SuppressNavigationTransitionInfo());
        }
        else
        {
            Nav.IsPaneVisible = true;
            var item = Nav.MenuItems.Concat(Nav.FooterMenuItems).OfType<NavigationViewItem>().FirstOrDefault(i => (string)i.Tag == page);
            if (file != null) EditorPage.PendingFile = file;
            if (item != null)
            {
                if (Nav.SelectedItem == item) Navigate(page);
                else Nav.SelectedItem = item;
            }
        }
        Activate();
        SetForegroundWindow(WindowNative.GetWindowHandle(this));
    }

    public void SetupFinished() => Show("captures");

    private void Navigate(string tag)
    {
        var type = tag switch
        {
            "editor" => typeof(EditorPage),
            "settings" => typeof(SettingsPage),
            "about" => typeof(AboutPage),
            _ => typeof(CapturesPage),
        };
        // No slide-in animation: pages switch instantly.
        ContentFrame.Navigate(type, null, new Microsoft.UI.Xaml.Media.Animation.SuppressNavigationTransitionInfo());
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem item) Navigate((string)item.Tag);
    }

    public void OpenInEditor(string file) => Show("editor", file);
}
