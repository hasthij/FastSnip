using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Text;
using Windows.Graphics.Imaging;
using Windows.Media.Ocr;
using Colors = Microsoft.UI.Colors;

namespace FastSnip.App.Services;

public sealed record BenchResult(string Key, string Name, string Where, bool Available, double MedianMs, double Accuracy, string Note);

/// <summary>
/// First-run speed test: reads the same sample images with each text engine
/// (5 runs after a warm-up) and keeps the fastest one whose accuracy is
/// within 2 points of the best. Runs in about 5 seconds.
/// </summary>
public static class OcrBench
{
    private static readonly (string Text, string Font, float Size, bool Mono)[] Samples =
    {
        ("Release notes for build 1.4. The overlay now opens in under 16 ms on most PCs.", "Segoe UI", 15, false),
        ("Save changes   Cancel   Open settings   Show in folder   Copy text", "Segoe UI", 12, false),
        ("winget install FastSnip.FastSnip --source msstore", "Cascadia Mono", 14, true),
        ("Questions? Write to support@example.com or visit example.com/help today.", "Segoe UI", 13, false),
    };

    public static async Task<List<BenchResult>> RunAsync(IProgress<double>? progress = null)
    {
        var images = Samples.Select(s => Render(s.Text, s.Font, s.Size)).ToList();
        var results = new List<BenchResult>();
        int step = 0, steps = 2 * images.Count * 6;
        void Tick() => progress?.Report(Math.Min(1.0, ++step / (double)steps));

        // Windows AI Text Recognizer (NPU). Needs a Copilot+ PC and the packaged app.
        results.Add(await TryTextRecognizer(images, Tick));
        // Windows.Media.Ocr: on every Windows 10/11.
        results.Add(await RunMediaOcr(images, Tick));
        progress?.Report(1);
        return results;
    }

    /// The engine the rule picks: fastest, unless more than 2 points less accurate than the best.
    public static BenchResult? Pick(IEnumerable<BenchResult> results)
    {
        var ok = results.Where(r => r.Available).ToList();
        if (ok.Count == 0) return null;
        var best = ok.Max(r => r.Accuracy);
        return ok.Where(r => r.Accuracy >= best - 2).OrderBy(r => r.MedianMs).First();
    }

    public static void Save(Config cfg, List<BenchResult> results)
    {
        var pick = Pick(results);
        cfg.Set("text", "engine_selected", pick?.Key ?? "media-ocr");
        foreach (var r in results)
        {
            cfg.Set("text", $"bench_{r.Key.Replace('-', '_')}_ms", r.Available ? Math.Round(r.MedianMs, 1) : -1.0);
        }
    }

    private static SoftwareBitmap Render(string text, string font, float size)
    {
        var device = CanvasDevice.GetSharedDevice();
        using var fmt = new CanvasTextFormat { FontFamily = font, FontSize = size, WordWrapping = CanvasWordWrapping.NoWrap };
        using var layout = new CanvasTextLayout(device, text, fmt, 4000, 200);
        var w = (int)Math.Ceiling(layout.LayoutBounds.Width) + 40;
        var h = (int)Math.Ceiling(layout.LayoutBounds.Height) + 30;
        using var target = new CanvasRenderTarget(device, w, h, 96);
        using (var ds = target.CreateDrawingSession())
        {
            ds.Clear(Colors.White);
            ds.DrawTextLayout(layout, 20, 15, Colors.Black);
        }
        var bytes = target.GetPixelBytes();
        return SoftwareBitmap.CreateCopyFromBuffer(bytes.AsBuffer(), BitmapPixelFormat.Bgra8, w, h, BitmapAlphaMode.Premultiplied);
    }

    private static async Task<BenchResult> RunMediaOcr(List<SoftwareBitmap> images, Action tick)
    {
        var engine = OcrEngine.TryCreateFromUserProfileLanguages();
        if (engine == null)
            return new BenchResult("media-ocr", "Windows.Media.Ocr", "CPU", false, 0, 0, "No OCR language is installed");
        async Task<string> Read(SoftwareBitmap b) => (await engine.RecognizeAsync(b)).Text;
        return await Measure("media-ocr", "Windows.Media.Ocr", "Runs on the CPU", images, Read, tick);
    }

    private static async Task<BenchResult> TryTextRecognizer(List<SoftwareBitmap> images, Action tick)
    {
        const string key = "text-recognizer", name = "Windows AI Text Recognizer", where = "Runs on the NPU";
        try
        {
            var state = Microsoft.Windows.AI.Imaging.TextRecognizer.GetReadyState();
            if (state == Microsoft.Windows.AI.AIFeatureReadyState.NotSupportedOnCurrentSystem)
            {
                for (int i = 0; i < images.Count * 6; i++) tick();
                return new BenchResult(key, name, where, false, 0, 0, "Needs a Copilot+ PC with an NPU");
            }
            if (state != Microsoft.Windows.AI.AIFeatureReadyState.Ready)
            {
                var ensure = await Microsoft.Windows.AI.Imaging.TextRecognizer.EnsureReadyAsync();
                if (ensure.Status != Microsoft.Windows.AI.AIFeatureReadyResultState.Success)
                {
                    for (int i = 0; i < images.Count * 6; i++) tick();
                    return new BenchResult(key, name, where, false, 0, 0, "Windows is still getting it ready");
                }
            }
            var rec = await Microsoft.Windows.AI.Imaging.TextRecognizer.CreateAsync();
            Task<string> Read(SoftwareBitmap b)
            {
                var buf = Microsoft.Graphics.Imaging.ImageBuffer.CreateForSoftwareBitmap(b);
                var r = rec.RecognizeTextFromImage(buf);
                return Task.FromResult(string.Join(" ", r.Lines.Select(l => l.Text)));
            }
            return await Measure(key, name, where, images, Read, tick);
        }
        catch (Exception ex)
        {
            for (int i = 0; i < images.Count * 6; i++) tick();
            var why = ex is COMException or UnauthorizedAccessException or TypeLoadException or InvalidCastException
                ? "Not available on this PC"
                : "Not available here";
            return new BenchResult(key, name, where, false, 0, 0, why);
        }
    }

    private static async Task<BenchResult> Measure(string key, string name, string where, List<SoftwareBitmap> images,
        Func<SoftwareBitmap, Task<string>> read, Action tick)
    {
        var times = new List<double>();
        double accSum = 0;
        for (int i = 0; i < images.Count; i++)
        {
            await read(images[i]); // warm-up
            tick();
            string last = "";
            for (int run = 0; run < 5; run++)
            {
                var sw = Stopwatch.StartNew();
                last = await read(images[i]);
                times.Add(sw.Elapsed.TotalMilliseconds);
                tick();
            }
            accSum += Accuracy(Samples[i].Text, last);
        }
        times.Sort();
        var median = times[times.Count / 2];
        return new BenchResult(key, name, where, true, median, Math.Round(accSum / images.Count * 100, 1), "");
    }

    /// 1 - character error rate, ignoring case and spacing differences.
    private static double Accuracy(string truth, string got)
    {
        static string Norm(string s) => string.Join(" ", s.ToLowerInvariant().Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
        var a = Norm(truth);
        var b = Norm(got);
        if (a.Length == 0) return 1;
        var d = new int[a.Length + 1, b.Length + 1];
        for (int i = 0; i <= a.Length; i++) d[i, 0] = i;
        for (int j = 0; j <= b.Length; j++) d[0, j] = j;
        for (int i = 1; i <= a.Length; i++)
            for (int j = 1; j <= b.Length; j++)
                d[i, j] = Math.Min(Math.Min(d[i - 1, j] + 1, d[i, j - 1] + 1), d[i - 1, j - 1] + (a[i - 1] == b[j - 1] ? 0 : 1));
        return Math.Max(0, 1 - d[a.Length, b.Length] / (double)a.Length);
    }
}
