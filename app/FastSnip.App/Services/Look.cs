using Microsoft.UI.Xaml;
using Windows.UI;

namespace FastSnip.App.Services;

/// <summary>Accent color and theme from the settings, applied to the whole window.</summary>
public static class Look
{
    public static readonly (string Key, string Name, Color Light, Color Dark)[] Presets =
    {
        ("teal", "Teal", Hex(0x0b7a73), Hex(0x3cc4b9)),
        ("blue", "Blue", Hex(0x0067c0), Hex(0x4cc2ff)),
        ("violet", "Violet", Hex(0x6b4eff), Hex(0xa594ff)),
        ("green", "Green", Hex(0x1d7f3a), Hex(0x5fd08f)),
        ("orange", "Orange", Hex(0xc25400), Hex(0xff9a4d)),
        ("pink", "Pink", Hex(0xc2185b), Hex(0xff7fb0)),
    };

    public static Color Hex(uint rgb) => Color.FromArgb(255, (byte)(rgb >> 16), (byte)(rgb >> 8), (byte)rgb);

    public static bool TryParseHex(string s, out Color c)
    {
        c = default;
        s = s.Trim().TrimStart('#');
        if (s.Length != 6 || !uint.TryParse(s, System.Globalization.NumberStyles.HexNumber, null, out var v)) return false;
        c = Hex(v);
        return true;
    }

    public static string ToHex(Color c) => $"#{c.R:X2}{c.G:X2}{c.B:X2}";

    private static Color Mix(Color a, Color b, double t) => Color.FromArgb(255,
        (byte)(a.R + (b.R - a.R) * t), (byte)(a.G + (b.G - a.G) * t), (byte)(a.B + (b.B - a.B) * t));

    /// The accent to use, or null to follow Windows.
    public static Color? Accent(Config cfg)
    {
        var a = cfg.GetString("look", "accent", "teal");
        if (a == "system") return null;
        var p = Presets.FirstOrDefault(x => x.Key == a);
        if (p.Key != null) return p.Light;
        return TryParseHex(a, out var c) ? c : Presets[0].Light;
    }

    private static readonly string[] AccentKeys =
    {
        "SystemAccentColor", "SystemAccentColorLight1", "SystemAccentColorLight2", "SystemAccentColorLight3",
        "SystemAccentColorDark1", "SystemAccentColorDark2", "SystemAccentColorDark3",
    };

    /// Override the system accent resources (or remove the overrides for Follow Windows).
    public static void ApplyAccent(ResourceDictionary res, Config cfg)
    {
        var accent = Accent(cfg);
        if (accent is not Color c)
        {
            // Follow Windows: drop our overrides so the system accent shows through.
            foreach (var k in AccentKeys) res.Remove(k);
            return;
        }
        var white = Hex(0xffffff);
        var black = Hex(0x000000);
        res["SystemAccentColor"] = c;
        res["SystemAccentColorLight1"] = Mix(c, white, 0.2);
        res["SystemAccentColorLight2"] = Mix(c, white, 0.4);
        res["SystemAccentColorLight3"] = Mix(c, white, 0.6);
        res["SystemAccentColorDark1"] = Mix(c, black, 0.15);
        res["SystemAccentColorDark2"] = Mix(c, black, 0.3);
        res["SystemAccentColorDark3"] = Mix(c, black, 0.45);
    }

    public static ElementTheme Theme(Config cfg) => cfg.GetString("look", "theme", "system") switch
    {
        "light" => ElementTheme.Light,
        "dark" => ElementTheme.Dark,
        _ => ElementTheme.Default,
    };
}
