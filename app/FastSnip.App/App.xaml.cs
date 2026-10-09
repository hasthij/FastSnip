using FastSnip.App.Services;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppLifecycle;

namespace FastSnip.App;

/// <summary>
/// Command line (sent by the core and the toast):
///   FastSnip.App.exe                 captures gallery
///   FastSnip.App.exe --edit FILE     open FILE in the editor
///   FastSnip.App.exe --settings      settings
/// Only one window runs; a second launch hands its request to it.
/// </summary>
public partial class App : Application
{
    public static MainWindow? Window { get; private set; }
    public static Config Settings { get; private set; } = Config.Load();

    public static string ErrorLog => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "FastSnip", "app-errors.log");

    public static void Log(object? ex) => LogError(ex);

    private static void LogError(object? ex)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(ErrorLog)!);
            File.AppendAllText(ErrorLog, $"{DateTime.Now:u} {ex}{Environment.NewLine}{Environment.NewLine}");
        }
        catch { }
    }

    public App()
    {
        UnhandledException += (_, e) => LogError(e.Exception);
        AppDomain.CurrentDomain.UnhandledException += (_, e) => LogError(e.ExceptionObject);
        InitializeComponent();
    }

    public static (string Page, string? File) ParseArgs(IEnumerable<string> args)
    {
        var a = args.ToList();
        int i = a.IndexOf("--edit");
        if (i >= 0 && i + 1 < a.Count) return ("editor", a[i + 1]);
        if (a.Contains("--settings")) return ("settings", null);
        if (a.Contains("--setup")) return ("setup", null);
        return ("captures", null);
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        var me = AppInstance.FindOrRegisterForKey("FastSnip.App");
        if (!me.IsCurrent)
        {
            // Another window is open: send it our request and quit.
            var act = AppInstance.GetCurrent().GetActivatedEventArgs();
            me.RedirectActivationToAsync(act).AsTask().Wait();
            Environment.Exit(0);
            return;
        }
        me.Activated += (_, e) =>
        {
            var cmd = e.Data is Windows.ApplicationModel.Activation.ILaunchActivatedEventArgs la ? la.Arguments : "";
            var parsed = ParseArgs(SplitArgs(cmd));
            Window?.DispatcherQueue.TryEnqueue(() => Window.Show(parsed.Page, parsed.File));
        };

        Look.ApplyAccent(Resources, Settings);
        Core.EnsureRunning();
        var (page, file) = ParseArgs(Environment.GetCommandLineArgs().Skip(1));
        if (!Settings.GetBool("setup", "done", false) && page == "captures") page = "setup";
        Window = new MainWindow();
        Window.Show(page, file);
    }

    private static IEnumerable<string> SplitArgs(string cmd)
    {
        var list = new List<string>();
        var cur = new System.Text.StringBuilder();
        bool quoted = false;
        foreach (var ch in cmd)
        {
            if (ch == '"') { quoted = !quoted; continue; }
            if (ch == ' ' && !quoted)
            {
                if (cur.Length > 0) { list.Add(cur.ToString()); cur.Clear(); }
                continue;
            }
            cur.Append(ch);
        }
        if (cur.Length > 0) list.Add(cur.ToString());
        return list;
    }

    public static void ReloadSettings() => Settings = Config.Load();
}
