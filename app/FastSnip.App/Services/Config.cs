using System.Globalization;
using System.Text;

namespace FastSnip.App.Services;

/// <summary>
/// Reads and writes %APPDATA%\FastSnip\config.toml, the file the Rust core
/// also reads. Only the small TOML subset FastSnip uses is supported:
/// [sections], strings, booleans, numbers and arrays of strings. Unknown keys
/// are kept, so the app never drops settings it doesn't know about.
/// </summary>
public sealed class Config
{
    public static string Dir => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "FastSnip");
    public static string FilePath => Path.Combine(Dir, "config.toml");

    private readonly List<(string Section, List<(string Key, object Value)> Items)> _sections = new();

    public static Config Load()
    {
        var c = new Config();
        try
        {
            if (File.Exists(FilePath)) c.Parse(File.ReadAllText(FilePath));
        }
        catch
        {
            // A broken file falls back to defaults, same as the core.
        }
        return c;
    }

    public void Save()
    {
        Directory.CreateDirectory(Dir);
        File.WriteAllText(FilePath, Serialize());
        Core.Reload();
    }

    // ----------------------------------------------------------- typed access with defaults

    public string GetString(string section, string key, string def) => Get(section, key) as string ?? def;
    public bool GetBool(string section, string key, bool def) => Get(section, key) is bool b ? b : def;
    public double GetNumber(string section, string key, double def) => Get(section, key) switch
    {
        long l => l,
        double d => d,
        _ => def,
    };
    public List<string> GetList(string section, string key, IEnumerable<string> def) =>
        Get(section, key) is List<string> l ? new List<string>(l) : new List<string>(def);

    public object? Get(string section, string key)
    {
        foreach (var (s, items) in _sections)
            if (s == section)
                foreach (var (k, v) in items)
                    if (k == key) return v;
        return null;
    }

    public void Set(string section, string key, object value)
    {
        if (value is int i) value = (long)i;
        if (value is float f) value = (double)f;
        var sec = _sections.FirstOrDefault(x => x.Section == section);
        if (sec.Items == null)
        {
            sec = (section, new List<(string, object)>());
            _sections.Add(sec);
        }
        for (int n = 0; n < sec.Items.Count; n++)
        {
            if (sec.Items[n].Key == key)
            {
                sec.Items[n] = (key, value);
                return;
            }
        }
        sec.Items.Add((key, value));
    }

    // ----------------------------------------------------------- TOML subset

    private void Parse(string text)
    {
        string section = "";
        var lines = text.Replace("\r\n", "\n").Split('\n');
        for (int i = 0; i < lines.Length; i++)
        {
            var line = StripComment(lines[i]).Trim();
            if (line.Length == 0) continue;
            if (line.StartsWith('[') && line.EndsWith(']'))
            {
                section = line[1..^1].Trim();
                continue;
            }
            int eq = line.IndexOf('=');
            if (eq <= 0) continue;
            var key = line[..eq].Trim();
            var raw = line[(eq + 1)..].Trim();
            // Multi-line arrays, as the Rust toml crate writes them.
            if (raw.StartsWith('[') && !raw.TrimEnd().EndsWith(']'))
            {
                var sb = new StringBuilder(raw);
                while (++i < lines.Length)
                {
                    var more = StripComment(lines[i]).Trim();
                    sb.Append(more);
                    if (more.EndsWith(']')) break;
                }
                raw = sb.ToString();
            }
            Set(section, key, ParseValue(raw));
        }
    }

    private static string StripComment(string line)
    {
        bool inStr = false;
        for (int i = 0; i < line.Length; i++)
        {
            if (line[i] == '"' && (i == 0 || line[i - 1] != '\\')) inStr = !inStr;
            if (line[i] == '#' && !inStr) return line[..i];
        }
        return line;
    }

    private static object ParseValue(string raw)
    {
        if (raw.StartsWith('"')) return Unquote(raw);
        if (raw == "true") return true;
        if (raw == "false") return false;
        if (raw.StartsWith('['))
        {
            var list = new List<string>();
            var inner = raw.Trim('[', ']');
            int pos = 0;
            while (pos < inner.Length)
            {
                int start = inner.IndexOf('"', pos);
                if (start < 0) break;
                int end = start + 1;
                while (end < inner.Length && !(inner[end] == '"' && inner[end - 1] != '\\')) end++;
                list.Add(Unquote(inner[start..Math.Min(end + 1, inner.Length)]));
                pos = end + 1;
            }
            return list;
        }
        if (long.TryParse(raw, NumberStyles.Integer, CultureInfo.InvariantCulture, out var l)) return l;
        if (double.TryParse(raw, NumberStyles.Float, CultureInfo.InvariantCulture, out var d)) return d;
        return raw;
    }

    private static string Unquote(string s)
    {
        s = s.Trim();
        if (s.Length >= 2 && s[0] == '"' && s[^1] == '"') s = s[1..^1];
        return s.Replace("\\\\", "\u0000").Replace("\\\"", "\"").Replace("\\n", "\n").Replace("\u0000", "\\");
    }

    private static string Quote(string s) => "\"" + s.Replace("\\", "\\\\").Replace("\"", "\\\"").Replace("\n", "\\n") + "\"";

    private static string Format(object v) => v switch
    {
        string s => Quote(s),
        bool b => b ? "true" : "false",
        long l => l.ToString(CultureInfo.InvariantCulture),
        double d => d.ToString("0.0###", CultureInfo.InvariantCulture),
        List<string> list => "[" + string.Join(", ", list.Select(Quote)) + "]",
        _ => Quote(v.ToString() ?? ""),
    };

    private string Serialize()
    {
        var sb = new StringBuilder();
        foreach (var (section, items) in _sections.Where(s => s.Section == ""))
            foreach (var (k, v) in items) sb.Append(k).Append(" = ").Append(Format(v)).Append('\n');
        foreach (var (section, items) in _sections.Where(s => s.Section != ""))
        {
            sb.Append('\n').Append('[').Append(section).Append("]\n");
            foreach (var (k, v) in items) sb.Append(k).Append(" = ").Append(Format(v)).Append('\n');
        }
        return sb.ToString();
    }
}
