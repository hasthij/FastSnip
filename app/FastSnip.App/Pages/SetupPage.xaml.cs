using FastSnip.App.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.Win32;

namespace FastSnip.App.Pages;

/// <summary>First run: background mode, shortcuts, text engine speed test, done.</summary>
public sealed partial class SetupPage : Page
{
    private int _step = 1;
    private bool _tested;
    private readonly string[] _names = { "Background", "Shortcuts", "Text speed test", "Done" };
    private Config C => App.Settings;

    public SetupPage()
    {
        InitializeComponent();
        ScOpen.SetItems(C.GetList("shortcuts", "open_toolbar", new[] { "PrintScreen", "Win+Shift+S" }));
        ScRecord.SetItems(C.GetList("shortcuts", "record_full_screen", new[] { "Win+Shift+R" }));
        ScText.SetItems(C.GetList("shortcuts", "grab_text", Array.Empty<string>()));
        ScOpen.Changed += l => C.Set("shortcuts", "open_toolbar", l);
        ScRecord.Changed += l => C.Set("shortcuts", "record_full_screen", l);
        ScText.Changed += l => C.Set("shortcuts", "grab_text", l);
        using var k = Registry.CurrentUser.OpenSubKey(@"Control Panel\Keyboard");
        PrtScInfo.IsOpen = k?.GetValue("PrintScreenKeyForSnippingEnabled") is int v && v == 1;
        Show();
    }

    private void Show()
    {
        Step1.Visibility = _step == 1 ? Visibility.Visible : Visibility.Collapsed;
        Step2.Visibility = _step == 2 ? Visibility.Visible : Visibility.Collapsed;
        Step3.Visibility = _step == 3 ? Visibility.Visible : Visibility.Collapsed;
        Step4.Visibility = _step == 4 ? Visibility.Visible : Visibility.Collapsed;
        Back.Visibility = _step > 1 && _step < 4 ? Visibility.Visible : Visibility.Collapsed;
        Next.Content = _step == 4 ? "Start using FastSnip" : "Continue";
        Steps.Children.Clear();
        for (int i = 1; i <= 4; i++)
        {
            var done = i < _step;
            var cur = i == _step;
            var dot = new Border
            {
                Width = 22,
                Height = 22,
                CornerRadius = new CornerRadius(11),
                BorderThickness = new Thickness(1.5),
                BorderBrush = (Brush)Application.Current.Resources[cur || done ? "AccentFillColorDefaultBrush" : "ControlStrokeColorDefaultBrush"],
                Background = done ? (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"] : null,
                Child = new TextBlock
                {
                    Text = done ? "✓" : i.ToString(),
                    FontSize = 11,
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = VerticalAlignment.Center,
                    Foreground = (Brush)Application.Current.Resources[done ? "TextOnAccentFillColorPrimaryBrush" : "TextFillColorPrimaryBrush"],
                },
            };
            var sp = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
            sp.Children.Add(dot);
            sp.Children.Add(new TextBlock
            {
                Text = _names[i - 1],
                VerticalAlignment = VerticalAlignment.Center,
                FontWeight = cur ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal,
                Foreground = (Brush)Application.Current.Resources[cur ? "TextFillColorPrimaryBrush" : "TextFillColorSecondaryBrush"],
            });
            Steps.Children.Add(sp);
        }
        if (_step == 3 && !_tested) _ = RunTest();
        if (_step == 4)
        {
            var first = C.GetList("shortcuts", "open_toolbar", new[] { "PrintScreen" }).FirstOrDefault() ?? "your shortcut";
            DoneText.Text = $"Press {Controls.ShortcutList.Pretty(first)} to open the toolbar.";
        }
    }

    private async Task RunTest()
    {
        _tested = true;
        Next.IsEnabled = false;
        Results.Children.Clear();
        var results = await OcrBench.RunAsync(new Progress<double>(p => TestBar.Value = p));
        OcrBench.Save(C, results);
        var pick = OcrBench.Pick(results);
        foreach (var r in results)
        {
            var g = new Grid { ColumnSpacing = 12, Padding = new Thickness(0, 6, 0, 6) };
            g.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            g.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(90) });
            g.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(90) });
            g.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(90) });
            var name = new StackPanel();
            name.Children.Add(new TextBlock { Text = r.Name, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            name.Children.Add(new TextBlock { Text = r.Available ? r.Where : r.Note, FontSize = 12, Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"] });
            g.Children.Add(name);
            void Cell(int col, string text, bool accent = false)
            {
                var t = new TextBlock { Text = text, VerticalAlignment = VerticalAlignment.Center };
                if (accent) t.Foreground = (Brush)Application.Current.Resources["AccentTextFillColorPrimaryBrush"];
                Grid.SetColumn(t, col);
                g.Children.Add(t);
            }
            Cell(1, r.Available ? $"{r.MedianMs:0} ms" : "—");
            Cell(2, r.Available ? $"{r.Accuracy:0.0}%" : "—");
            Cell(3, pick == r ? "Selected" : r.Available ? "Fallback" : "Not available", pick == r);
            Results.Children.Add(g);
        }
        TestStatus.Text = pick == null
            ? "No text engine is available. Install a language with text recognition in Windows Settings > Time & language."
            : $"{pick.Name} reads text in about {pick.MedianMs:0} ms on this PC.";
        Next.IsEnabled = true;
    }

    private async void OpenKeyboardSettings_Click(object sender, RoutedEventArgs e) =>
        await Windows.System.Launcher.LaunchUriAsync(new Uri("ms-settings:easeofaccess-keyboard"));

    private void Back_Click(object sender, RoutedEventArgs e)
    {
        _step = Math.Max(1, _step - 1);
        Show();
    }

    private async void Next_Click(object sender, RoutedEventArgs e)
    {
        if (_step == 1)
        {
            C.Set("", "background", BgDemand.IsChecked == true ? "on-demand" : "tray");
            await Startup.SetAsync(StartWithWindows.IsChecked == true);
        }
        if (_step < 4)
        {
            _step++;
            Show();
            return;
        }
        C.Set("setup", "done", true);
        C.Save();
        App.Window?.SetupFinished();
    }
}
