using System.Collections.ObjectModel;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage;
using Windows.Storage.FileProperties;

namespace FastSnip.App.Services;

public sealed class CaptureItem : System.ComponentModel.INotifyPropertyChanged
{
    public event System.ComponentModel.PropertyChangedEventHandler? PropertyChanged;
    private void Changed(string name) => PropertyChanged?.Invoke(this, new(name));

    public required string Path { get; init; }
    public required DateTime When { get; init; }
    public bool IsVideo { get; init; }
    public string Kind => IsVideo ? "MP4" : "PNG";
    public string Time => When.ToString("HH:mm");
    private string _badge = "";
    public string Badge { get => _badge; set { _badge = value; Changed(nameof(Badge)); } }
    private BitmapImage? _thumb;
    public BitmapImage? Thumb { get => _thumb; set { _thumb = value; Changed(nameof(Thumb)); } }
    public string Name => System.IO.Path.GetFileName(Path);
    /// Words read from the screenshot at capture time (for search).
    public string Text { get; set; } = "";
}

public sealed class DayGroup : ObservableCollection<CaptureItem>
{
    public required string Title { get; init; }
}

/// <summary>Everything in the screenshot and recording folders, newest first.</summary>
public static class Captures
{
    public static string ScreenshotsDir(Config c)
    {
        var custom = c.GetString("saving", "screenshots_dir", "");
        if (custom.Length > 0) return custom;
        var pics = Environment.GetFolderPath(Environment.SpecialFolder.MyPictures);
        return System.IO.Path.Combine(pics, "Screenshots");
    }

    public static string RecordingsDir(Config c)
    {
        var custom = c.GetString("saving", "recordings_dir", "");
        if (custom.Length > 0) return custom;
        var vids = Environment.GetFolderPath(Environment.SpecialFolder.MyVideos);
        return System.IO.Path.Combine(vids, "Screen Recordings");
    }

    /// Text the core saved for each screenshot, keyed by file name.
    public static string TextDir => System.IO.Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "FastSnip", "text");

    public static List<CaptureItem> List(Config c)
    {
        var items = new List<CaptureItem>();
        void Add(string dir, string pattern, bool video)
        {
            if (!Directory.Exists(dir)) return;
            foreach (var f in Directory.EnumerateFiles(dir, pattern))
            {
                var info = new FileInfo(f);
                var item = new CaptureItem { Path = f, When = info.LastWriteTime, IsVideo = video };
                if (!video)
                {
                    var t = System.IO.Path.Combine(TextDir, info.Name + ".txt");
                    if (File.Exists(t)) item.Text = File.ReadAllText(t);
                }
                items.Add(item);
            }
        }
        Add(ScreenshotsDir(c), "*.png", false);
        Add(RecordingsDir(c), "*.mp4", true);
        return items.OrderByDescending(i => i.When).ToList();
    }

    public static string DayTitle(DateTime d)
    {
        var today = DateTime.Today;
        if (d.Date == today) return "Today";
        if (d.Date == today.AddDays(-1)) return "Yesterday";
        if (d.Date > today.AddDays(-7)) return d.ToString("dddd");
        return d.ToString(d.Year == today.Year ? "MMMM d" : "MMMM d, yyyy");
    }

    // A few at a time, so a big folder never makes the window stutter.
    private static readonly SemaphoreSlim Gate = new(3);

    public static async Task LoadThumbAsync(CaptureItem item)
    {
        await Gate.WaitAsync();
        try
        {
            var file = await StorageFile.GetFileFromPathAsync(item.Path);
            var bmp = new BitmapImage { DecodePixelWidth = 400 };
            if (item.IsVideo)
            {
                using var thumb = await file.GetThumbnailAsync(ThumbnailMode.VideosView, 400, ThumbnailOptions.ResizeThumbnail);
                if (thumb != null) await bmp.SetSourceAsync(thumb);
            }
            else
            {
                // Decode the PNG itself, shrunk while decoding: a real preview, not the shell's file icon.
                using var stream = await file.OpenReadAsync();
                await bmp.SetSourceAsync(stream);
            }
            item.Thumb = bmp;
            if (item.IsVideo)
            {
                var props = await file.Properties.GetVideoPropertiesAsync();
                var d = props.Duration;
                item.Badge = d.TotalHours >= 1 ? d.ToString(@"h\:mm\:ss") : d.ToString(@"m\:ss");
            }
            else
            {
                var props = await file.Properties.GetImagePropertiesAsync();
                item.Badge = $"{props.Width}×{props.Height}";
            }
        }
        catch
        {
            // A file that can't be read just shows without a thumbnail.
        }
        finally
        {
            Gate.Release();
        }
    }

    /// Move to the Recycle Bin, so it can be restored.
    public static async Task RecycleAsync(string path)
    {
        var file = await StorageFile.GetFileFromPathAsync(path);
        await file.DeleteAsync(StorageDeleteOption.Default);
    }
}
