using FastSnip.App.Controls;
using FastSnip.App.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Microsoft.Win32;
using Windows.Storage.Pickers;
using Windows.UI;

namespace FastSnip.App.Pages;

/// <summary>
/// Every change is written to config.toml straight away and the core is told
/// to reload, so there's no Save button.
/// </summary>
public sealed partial class SettingsPage : Page
{
    private bool _loading = true;
    private Config C => App.Settings;

    public SettingsPage()
    {
        InitializeComponent();
        Loaded += async (_, _) => await LoadAll();
    }

    private static void Select(ComboBox box, string tag)
    {
        box.SelectedItem = box.Items.OfType<ComboBoxItem>().FirstOrDefault(i => (string)i.Tag == tag) ?? box.Items[0];
    }

    private static string Tag(ComboBox box) => (box.SelectedItem as ComboBoxItem)?.Tag as string ?? "";

    private async Task LoadAll()
    {
        _loading = true;
        (C.GetString("", "background", "tray") == "on-demand" ? BgDemand : BgTray).IsChecked = true;
        StartupToggle.IsOn = await Startup.IsOnAsync();

        void Shortcut(ShortcutList list, string key, string[] def)
        {
            list.SetItems(C.GetList("shortcuts", key, def));
            list.Changed += items =>
            {
                C.Set("shortcuts", key, items);
                C.Save();
            };
        }
        Shortcut(ScOpen, "open_toolbar", new[] { "PrintScreen", "Win+Shift+S" });
        Shortcut(ScFull, "snip_full_screen", Array.Empty<string>());
        Shortcut(ScRecord, "record_full_screen", new[] { "Win+Shift+R" });
        Shortcut(ScText, "grab_text", Array.Empty<string>());
        PrtScInfo.IsOpen = WindowsPrintScreenOn();

        Select(Fps, ((long)C.GetNumber("recording", "fps", 60)).ToString());
        Select(Quality, C.GetString("recording", "quality", "high"));
        Mic.IsOn = C.GetBool("recording", "microphone", true);
        SysAudio.IsOn = C.GetBool("recording", "system_audio", true);
        Cursor.IsOn = C.GetBool("recording", "show_cursor", true);
        Select(Countdown, ((long)C.GetNumber("recording", "countdown", 3)).ToString());
        UpdateQualityHint();

        Select(Engine, C.GetString("text", "engine", "auto"));
        ReadOnFreeze.IsOn = C.GetBool("text", "read_on_freeze", true);
        KeepLines.IsOn = C.GetBool("text", "keep_line_breaks", false);
        UpdateEngineHint();

        ShotsDir.Text = Captures.ScreenshotsDir(C);
        RecsDir.Text = Captures.RecordingsDir(C);
        Toast.IsOn = C.GetBool("saving", "show_toast", true);

        Select(Theme, C.GetString("look", "theme", "system"));
        Anim.IsOn = C.GetBool("look", "animations", true);
        AnimSpeed.Value = C.GetNumber("look", "animation_speed", 1.0);
        AnimSpeedRow.Visibility = Anim.IsOn ? Visibility.Visible : Visibility.Collapsed;
        UpdateAnimHint();
        Magnifier.IsOn = C.GetBool("look", "magnifier", true);
        BuildAccentRow();
        _loading = false;
        // Loading values can move focus into the page; always start at the top.
        DispatcherQueue.TryEnqueue(() => Scroller.ChangeView(null, 0, null, true));
    }

    private void Save()
    {
        if (!_loading) C.Save();
    }

    // ------------------------------------------------------------ background

    private void Bg_Checked(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        C.Set("", "background", BgDemand.IsChecked == true ? "on-demand" : "tray");
        Save();
    }

    private async void Startup_Toggled(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        var result = await Startup.SetAsync(StartupToggle.IsOn);
        if (result != StartupToggle.IsOn)
        {
            _loading = true;
            StartupToggle.IsOn = result;
            _loading = false;
            StartupHint.Text = "Windows blocked this. Turn FastSnip on in Task Manager > Startup apps.";
        }
    }

    // ------------------------------------------------------------ shortcuts

    /// Windows 11's own "Print Screen opens Snipping Tool" setting.
    private static bool WindowsPrintScreenOn()
    {
        using var k = Registry.CurrentUser.OpenSubKey(@"Control Panel\Keyboard");
        return k?.GetValue("PrintScreenKeyForSnippingEnabled") is int v && v == 1;
    }

    private async void OpenKeyboardSettings_Click(object sender, RoutedEventArgs e) =>
        await Windows.System.Launcher.LaunchUriAsync(new Uri("ms-settings:easeofaccess-keyboard"));

    // ------------------------------------------------------------ recording

    private void Rec_Changed(object sender, SelectionChangedEventArgs e)
    {
        if (_loading) return;
        C.Set("recording", "fps", long.Parse(Tag(Fps)));
        C.Set("recording", "quality", Tag(Quality));
        C.Set("recording", "countdown", long.Parse(Tag(Countdown)));
        UpdateQualityHint();
        Save();
    }

    private void Rec_Toggled(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        C.Set("recording", "microphone", Mic.IsOn);
        C.Set("recording", "system_audio", SysAudio.IsOn);
        C.Set("recording", "show_cursor", Cursor.IsOn);
        Save();
    }

    private void UpdateQualityHint()
    {
        // Same bits-per-pixel rule the core uses for the bitrate.
        var fps = double.Parse(Tag(Fps) is { Length: > 0 } f ? f : "60");
        var bpp = Tag(Quality) switch { "low" => 0.03, "standard" => 0.06, _ => 0.1 };
        var mbps = Math.Clamp(Math.Round(2560 * 1440 * fps * bpp / 1_000_000), 2, 120);
        QualityHint.Text = $"About {mbps * 60 / 8:0} MB per minute for a 1440p display at {fps:0} fps";
    }

    // ------------------------------------------------------------ text

    private void Text_Changed(object sender, SelectionChangedEventArgs e)
    {
        if (_loading) return;
        C.Set("text", "engine", Tag(Engine));
        UpdateEngineHint();
        Save();
    }

    private void Text_Toggled(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        C.Set("text", "read_on_freeze", ReadOnFreeze.IsOn);
        C.Set("text", "keep_line_breaks", KeepLines.IsOn);
        Save();
    }

    private void UpdateEngineHint()
    {
        var sel = C.GetString("text", "engine_selected", "");
        var ms = sel switch
        {
            "text-recognizer" => C.GetNumber("text", "bench_text_recognizer_ms", -1),
            "media-ocr" => C.GetNumber("text", "bench_media_ocr_ms", -1),
            _ => -1,
        };
        var name = sel == "text-recognizer" ? "Windows AI Text Recognizer" : "Windows.Media.Ocr";
        EngineHint.Text = sel.Length == 0
            ? "Not tested yet. Run the test to pick the fastest engine for this PC."
            : $"Speed test picked {name} · {ms:0} ms on this PC";
    }

    private async void RunTest_Click(object sender, RoutedEventArgs e)
    {
        RunTest.IsEnabled = false;
        TestProgress.Visibility = Visibility.Visible;
        try
        {
            var results = await OcrBench.RunAsync(new Progress<double>(p => TestProgress.Value = p));
            OcrBench.Save(C, results);
            C.Save();
            UpdateEngineHint();
        }
        finally
        {
            RunTest.IsEnabled = true;
            TestProgress.Visibility = Visibility.Collapsed;
        }
    }

    // ------------------------------------------------------------ saving

    private async void ChangeDir_Click(object sender, RoutedEventArgs e)
    {
        var key = (string)((FrameworkElement)sender).Tag;
        var picker = new FolderPicker();
        picker.FileTypeFilter.Add("*");
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(App.Window!));
        var folder = await picker.PickSingleFolderAsync();
        if (folder == null) return;
        C.Set("saving", key, folder.Path);
        Save();
        ShotsDir.Text = Captures.ScreenshotsDir(C);
        RecsDir.Text = Captures.RecordingsDir(C);
    }

    private void Saving_Toggled(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        C.Set("saving", "show_toast", Toast.IsOn);
        Save();
    }

    // ------------------------------------------------------------ look

    private void Look_Changed(object sender, SelectionChangedEventArgs e)
    {
        if (_loading) return;
        C.Set("look", "theme", Tag(Theme));
        Save();
        App.Window?.ApplyTheme();
    }

    private void Look_Toggled(object sender, RoutedEventArgs e)
    {
        if (_loading) return;
        C.Set("look", "animations", Anim.IsOn);
        C.Set("look", "magnifier", Magnifier.IsOn);
        AnimSpeedRow.Visibility = Anim.IsOn ? Visibility.Visible : Visibility.Collapsed;
        Save();
    }

    private DispatcherTimer? _speedSave;

    private void AnimSpeed_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        UpdateAnimHint();
        if (_loading) return;
        C.Set("look", "animation_speed", Math.Round(e.NewValue, 2));
        // Save once the slider stops moving.
        _speedSave ??= new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(300) };
        _speedSave.Stop();
        _speedSave.Tick -= SpeedTick;
        _speedSave.Tick += SpeedTick;
        _speedSave.Start();
    }

    private void SpeedTick(object? s, object e)
    {
        _speedSave?.Stop();
        Save();
    }

    private void UpdateAnimHint()
    {
        if (AnimSpeedHint == null) return;
        var v = AnimSpeed.Value;
        AnimSpeedHint.Text = $"{v:0.##}× · toolbar pops in over {140 / v:0} ms";
    }

    private void BuildAccentRow()
    {
        AccentRow.Children.Clear();
        var current = C.GetString("look", "accent", "teal");
        Button Swatch(Color c, string key, string name)
        {
            var b = new Button
            {
                Width = 26,
                Height = 26,
                Padding = new Thickness(0),
                CornerRadius = new CornerRadius(13),
                Background = new SolidColorBrush(c),
                BorderThickness = new Thickness(key == current ? 3 : 1),
                BorderBrush = key == current
                    ? (Brush)Application.Current.Resources["TextFillColorPrimaryBrush"]
                    : (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            };
            ToolTipService.SetToolTip(b, name);
            b.Click += (_, _) => SetAccent(key);
            return b;
        }
        foreach (var p in Look.Presets) AccentRow.Children.Add(Swatch(p.Light, p.Key, p.Name));

        var sys = new ToggleButton { Content = "Follow Windows", IsChecked = current == "system", Padding = new Thickness(10, 3, 10, 4) };
        sys.Click += (_, _) => SetAccent("system");
        AccentRow.Children.Add(sys);

        var custom = new Button { Padding = new Thickness(8, 3, 8, 4) };
        var isCustom = current.StartsWith('#');
        custom.Content = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            Children =
            {
                new Border { Width = 14, Height = 14, CornerRadius = new CornerRadius(3),
                    Background = new SolidColorBrush(isCustom && Look.TryParseHex(current, out var cc) ? cc : Look.Hex(0x888888)) },
                new TextBlock { Text = "Custom" },
            },
        };
        var picker = new ColorPicker { IsAlphaEnabled = false, IsMoreButtonVisible = false, ColorSpectrumShape = ColorSpectrumShape.Ring };
        if (isCustom && Look.TryParseHex(current, out var start)) picker.Color = start;
        var apply = new Button { Content = "Use this color", HorizontalAlignment = HorizontalAlignment.Right, Style = (Style)Application.Current.Resources["AccentButtonStyle"] };
        var flyout = new Flyout { Content = new StackPanel { Spacing = 8, Children = { picker, apply } } };
        apply.Click += (_, _) =>
        {
            flyout.Hide();
            SetAccent(Look.ToHex(picker.Color));
        };
        custom.Flyout = flyout;
        AccentRow.Children.Add(custom);

        AccentHint.Text = current switch
        {
            "system" => "Follows your Windows accent color",
            var k when k.StartsWith('#') => $"Custom {k.ToUpperInvariant()}",
            var k => (Look.Presets.FirstOrDefault(p => p.Key == k).Name ?? "Teal") + ". The window picks up a new accent the next time it opens.",
        };
    }

    private void SetAccent(string key)
    {
        C.Set("look", "accent", key);
        Save();
        BuildAccentRow();
    }
}
