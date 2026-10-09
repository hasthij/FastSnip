using Windows.Security.Authorization.AppCapabilityAccess;

namespace FastSnip.App.Services;

/// <summary>
/// Windows' per-app microphone permission (Settings > Privacy & security > Microphone).
/// When it's off, recordings get silence from the mic instead of an error.
/// </summary>
public static class MicAccess
{
    public static bool Blocked()
    {
        try
        {
            var s = AppCapability.Create("microphone").CheckAccess();
            return s is AppCapabilityAccessStatus.DeniedByUser or AppCapabilityAccessStatus.DeniedBySystem;
        }
        catch
        {
            return false; // Not packaged (development) or not available: nothing to warn about.
        }
    }

    /// Shows Windows' "allow microphone" prompt if it hasn't been answered yet.
    public static async Task RequestAsync()
    {
        try
        {
            var cap = AppCapability.Create("microphone");
            if (cap.CheckAccess() == AppCapabilityAccessStatus.UserPromptRequired)
                await cap.RequestAccessAsync();
        }
        catch
        {
        }
    }

    public static Task OpenSettingsAsync() =>
        Windows.System.Launcher.LaunchUriAsync(new Uri("ms-settings:privacy-microphone")).AsTask();
}
