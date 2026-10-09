using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;

namespace FastSnip.App;

/// <summary>Custom entry point so startup failures are written to the error log.</summary>
public static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        if (Environment.GetEnvironmentVariable("FASTSNIP_APP_TRACE") != null)
        {
            AppDomain.CurrentDomain.FirstChanceException += (_, e) => App.Log("first chance: " + e.Exception.Message + Environment.NewLine + Environment.StackTrace);
        }
        try
        {
            WinRT.ComWrappersSupport.InitializeComWrappers();
            Application.Start(p =>
            {
                var ctx = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
                SynchronizationContext.SetSynchronizationContext(ctx);
                _ = new App();
            });
            return 0;
        }
        catch (Exception ex)
        {
            App.Log("startup: " + ex);
            return 1;
        }
    }
}
