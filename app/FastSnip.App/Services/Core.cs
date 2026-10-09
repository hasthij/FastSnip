using System.Diagnostics;

namespace FastSnip.App.Services;

/// <summary>Talks to the Rust core (fastsnip.exe) through its command line.</summary>
public static class Core
{
    /// fastsnip.exe sits next to this app in the package, or in core\target\release during development.
    public static string? ExePath
    {
        get
        {
            var dir = AppContext.BaseDirectory;
            foreach (var candidate in new[]
            {
                Path.Combine(dir, "fastsnip.exe"),
                Path.Combine(dir, "..", "fastsnip.exe"),
                Path.GetFullPath(Path.Combine(dir, "..", "..", "..", "..", "..", "..", "core", "target", "release", "fastsnip.exe")),
            })
            {
                if (File.Exists(candidate)) return Path.GetFullPath(candidate);
            }
            return null;
        }
    }

    public static void Run(params string[] args)
    {
        var exe = ExePath;
        if (exe == null) return;
        try
        {
            var psi = new ProcessStartInfo(exe) { UseShellExecute = false, CreateNoWindow = true };
            foreach (var a in args) psi.ArgumentList.Add(a);
            Process.Start(psi);
        }
        catch
        {
            // The core not starting must never crash the window.
        }
    }

    /// Tell the core to re-read the settings file.
    public static void Reload() => Run("--reload");

    /// Make sure the core (or its listener) is running, in the configured mode.
    public static void EnsureRunning() => Run();

    public static void NewSnip() => Run("--open");
    public static void GrabText() => Run("--text");
    public static void Record() => Run("--record");
}
