using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;
using Windows.UI.Core;

namespace FastSnip.App.Controls;

/// <summary>
/// A row of shortcut chips (e.g. "PrtSc", "Win + Shift + S") with remove
/// buttons and an "Add" button that records the next key combination.
/// Text format matches the core's parser: "Win+Shift+S", "PrintScreen".
/// </summary>
public sealed class ShortcutList : StackPanel
{
    private List<string> _items = new();
    private Button? _recording;
    public event Action<List<string>>? Changed;
    public bool Single { get; set; }

    public ShortcutList()
    {
        Orientation = Orientation.Horizontal;
        Spacing = 6;
    }

    public void SetItems(IEnumerable<string> items)
    {
        _items = items.ToList();
        Rebuild();
    }

    public static string Pretty(string spec) => string.Join(" + ", spec.Split('+').Select(p => p.Trim() switch
    {
        "PrintScreen" => "PrtSc",
        var x => x,
    }));

    private void Rebuild()
    {
        Children.Clear();
        foreach (var item in _items)
        {
            var chip = new Button { Padding = new Thickness(10, 3, 6, 3) };
            var sp = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
            sp.Children.Add(new TextBlock { Text = Pretty(item), FontFamily = (Microsoft.UI.Xaml.Media.FontFamily)Application.Current.Resources["MonoFont"], FontSize = 12, VerticalAlignment = VerticalAlignment.Center });
            sp.Children.Add(new FontIcon { Glyph = "", FontSize = 10, VerticalAlignment = VerticalAlignment.Center });
            chip.Content = sp;
            ToolTipService.SetToolTip(chip, "Remove");
            var captured = item;
            chip.Click += (_, _) =>
            {
                _items.Remove(captured);
                Rebuild();
                Changed?.Invoke(_items);
            };
            Children.Add(chip);
        }
        if (Single && _items.Count >= 1) return;
        var add = new Button { Padding = new Thickness(10, 3, 10, 3) };
        add.Content = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 6,
            Children = { new FontIcon { Glyph = "", FontSize = 11 }, new TextBlock { Text = "Add", FontSize = 12 } },
        };
        add.Click += (_, _) => StartRecording(add);
        add.KeyDown += OnKeyDown;
        add.KeyUp += OnKeyUp;
        add.LostFocus += (_, _) => StopRecording();
        Children.Add(add);
    }

    private void StartRecording(Button b)
    {
        _recording = b;
        b.Content = new TextBlock { Text = "Press keys…", FontSize = 12 };
        b.Focus(FocusState.Programmatic);
    }

    private void StopRecording()
    {
        if (_recording == null) return;
        _recording = null;
        Rebuild();
    }

    private static bool Down(VirtualKey k) =>
        InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(CoreVirtualKeyStates.Down);

    private static string? KeyName(VirtualKey k) => k switch
    {
        >= VirtualKey.A and <= VirtualKey.Z => ((char)('A' + (k - VirtualKey.A))).ToString(),
        >= VirtualKey.Number0 and <= VirtualKey.Number9 => ((char)('0' + (k - VirtualKey.Number0))).ToString(),
        >= VirtualKey.F1 and <= VirtualKey.F24 => "F" + (k - VirtualKey.F1 + 1),
        VirtualKey.Snapshot => "PrintScreen",
        VirtualKey.Space => "Space",
        VirtualKey.Insert => "Insert",
        VirtualKey.Pause => "Pause",
        VirtualKey.Scroll => "ScrollLock",
        VirtualKey.Home => "Home",
        VirtualKey.End => "End",
        _ => null,
    };

    private void Accept(VirtualKey key)
    {
        var name = KeyName(key);
        if (name == null) return;
        var parts = new List<string>();
        if (Down(VirtualKey.LeftWindows) || Down(VirtualKey.RightWindows)) parts.Add("Win");
        if (Down(VirtualKey.Control)) parts.Add("Ctrl");
        if (Down(VirtualKey.Shift)) parts.Add("Shift");
        if (Down(VirtualKey.Menu)) parts.Add("Alt");
        // Plain letters would fire while typing anywhere: require a modifier, except PrtSc and F-keys.
        if (parts.Count == 0 && name.Length == 1) return;
        parts.Add(name);
        var spec = string.Join("+", parts);
        if (!_items.Contains(spec)) _items.Add(spec);
        _recording = null;
        Rebuild();
        Changed?.Invoke(_items);
    }

    private void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (_recording == null) return;
        e.Handled = true;
        if (e.Key == VirtualKey.Escape) { StopRecording(); return; }
        if (e.Key is VirtualKey.Shift or VirtualKey.Control or VirtualKey.Menu or VirtualKey.LeftWindows or VirtualKey.RightWindows
            or VirtualKey.LeftShift or VirtualKey.RightShift or VirtualKey.LeftControl or VirtualKey.RightControl or VirtualKey.LeftMenu or VirtualKey.RightMenu)
            return;
        Accept(e.Key);
    }

    // Print Screen often only arrives as a key-up.
    private void OnKeyUp(object sender, KeyRoutedEventArgs e)
    {
        if (_recording != null && e.Key == VirtualKey.Snapshot)
        {
            e.Handled = true;
            Accept(e.Key);
        }
    }
}
