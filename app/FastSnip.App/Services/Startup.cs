using Microsoft.Win32;

namespace FastSnip.App.Services;

/// <summary>
/// "Start FastSnip when I sign in". In the MSIX package this is the package's
/// startup task; when running unpackaged (development) it's the Run key.
/// </summary>
public static class Startup
{
    private const string TaskId = "FastSnipStartup";
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";

    private static bool Packaged
    {
        get
        {
            try { return Windows.ApplicationModel.Package.Current != null; }
            catch { return false; }
        }
    }

    public static async Task<bool> IsOnAsync()
    {
        if (Packaged)
        {
            var t = await Windows.ApplicationModel.StartupTask.GetAsync(TaskId);
            return t.State is Windows.ApplicationModel.StartupTaskState.Enabled or Windows.ApplicationModel.StartupTaskState.EnabledByPolicy;
        }
        using var k = Registry.CurrentUser.OpenSubKey(RunKey);
        return k?.GetValue("FastSnip") != null;
    }

    /// Returns the resulting state (Windows may refuse if the user turned it off in Task Manager).
    public static async Task<bool> SetAsync(bool on)
    {
        if (Packaged)
        {
            var t = await Windows.ApplicationModel.StartupTask.GetAsync(TaskId);
            if (on) return await t.RequestEnableAsync() == Windows.ApplicationModel.StartupTaskState.Enabled;
            t.Disable();
            return false;
        }
        using var k = Registry.CurrentUser.CreateSubKey(RunKey);
        if (on && Core.ExePath is string exe) k.SetValue("FastSnip", $"\"{exe}\"");
        else k.DeleteValue("FastSnip", false);
        return on;
    }
}
