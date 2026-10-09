using FastSnip.App.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Navigation;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage;

namespace FastSnip.App.Pages;

public sealed partial class CapturesPage : Page
{
    private List<CaptureItem> _all = new();
    private FileSystemWatcher? _shots, _videos;

    private bool _loaded;

    public CapturesPage()
    {
        InitializeComponent();
        // Keep the page (and its previews) alive between visits: switching back is instant.
        NavigationCacheMode = NavigationCacheMode.Required;
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        if (_loaded) return;
        _loaded = true;
        Reload();
        Watch();
    }

    private void Watch()
    {
        FileSystemWatcher? Make(string dir, string filter)
        {
            if (!Directory.Exists(dir)) return null;
            var w = new FileSystemWatcher(dir, filter) { EnableRaisingEvents = true };
            FileSystemEventHandler h = (_, _) => DispatcherQueue.TryEnqueue(Reload);
            w.Created += h;
            w.Deleted += h;
            w.Renamed += (_, _) => DispatcherQueue.TryEnqueue(Reload);
            return w;
        }
        _shots = Make(Captures.ScreenshotsDir(App.Settings), "*.png");
        _videos = Make(Captures.RecordingsDir(App.Settings), "*.mp4");
    }

    private readonly Dictionary<string, CaptureItem> _known = new();

    private void Reload()
    {
        // Keep items (and their loaded previews) that are still there.
        var fresh = Captures.List(App.Settings);
        _all = fresh.Select(i => _known.TryGetValue(i.Path, out var old) ? old : i).ToList();
        _known.Clear();
        foreach (var i in _all) _known[i.Path] = i;
        Apply();
        foreach (var item in _all.Where(i => i.Thumb == null).Take(300)) _ = LoadThumb(item);
    }

    private Task LoadThumb(CaptureItem item) => Captures.LoadThumbAsync(item);

    private bool _pending;

    private void Apply()
    {
        if (_pending) return;
        _pending = true;
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            _pending = false;
            IEnumerable<CaptureItem> items = _all;
            if (Filter.SelectedItem == FilterImages) items = items.Where(i => !i.IsVideo);
            if (Filter.SelectedItem == FilterVideos) items = items.Where(i => i.IsVideo);
            var q = Search.Text.Trim();
            if (q.Length > 0)
                items = items.Where(i => i.Text.Contains(q, StringComparison.OrdinalIgnoreCase) || i.Name.Contains(q, StringComparison.OrdinalIgnoreCase));
            var groups = items.GroupBy(i => i.When.Date).Select(g =>
            {
                var dg = new DayGroup { Title = Captures.DayTitle(g.Key) };
                foreach (var i in g) dg.Add(i);
                return dg;
            }).ToList();
            Groups.Source = groups;
            Empty.Visibility = groups.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            EmptyHint.Text = q.Length > 0
                ? "No screenshots contain that text."
                : "Press Print Screen or Win + Shift + S. Screenshots and recordings show up here.";
        });
    }

    private void Filter_SelectionChanged(SelectorBar sender, SelectorBarSelectionChangedEventArgs args) => Apply();

    private void Search_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args) => Apply();

    private void NewSnip_Click(object sender, RoutedEventArgs e) => Core.NewSnip();

    private void Record_Click(object sender, RoutedEventArgs e) => Core.Record();

    private async void OpenFolder_Click(object sender, RoutedEventArgs e)
    {
        var dir = Captures.ScreenshotsDir(App.Settings);
        Directory.CreateDirectory(dir);
        await Windows.System.Launcher.LaunchFolderPathAsync(dir);
    }

    private void Grid_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is CaptureItem item) App.Window?.OpenInEditor(item.Path);
    }

    private void Grid_RightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        if ((e.OriginalSource as FrameworkElement)?.DataContext is not CaptureItem item) return;
        var menu = new MenuFlyout();
        void Add(string text, string glyph, Func<Task> act)
        {
            var mi = new MenuFlyoutItem { Text = text, Icon = new FontIcon { Glyph = glyph } };
            mi.Click += async (_, _) => await act();
            menu.Items.Add(mi);
        }
        Add("Open in editor", "", () => { App.Window?.OpenInEditor(item.Path); return Task.CompletedTask; });
        Add("Copy", "", async () =>
        {
            var file = await StorageFile.GetFileFromPathAsync(item.Path);
            var dp = new DataPackage();
            if (item.IsVideo) dp.SetStorageItems(new[] { file });
            else dp.SetBitmap(Windows.Storage.Streams.RandomAccessStreamReference.CreateFromFile(file));
            Clipboard.SetContent(dp);
        });
        if (!item.IsVideo && item.Text.Length > 0)
            Add("Copy text", "", () =>
            {
                var dp = new DataPackage();
                dp.SetText(item.Text);
                Clipboard.SetContent(dp);
                return Task.CompletedTask;
            });
        Add("Show in folder", "", async () =>
        {
            // Opens the user's file manager with the capture selected.
            var file = await StorageFile.GetFileFromPathAsync(item.Path);
            var opts = new Windows.System.FolderLauncherOptions();
            opts.ItemsToSelect.Add(file);
            await Windows.System.Launcher.LaunchFolderPathAsync(Path.GetDirectoryName(item.Path)!, opts);
        });
        menu.Items.Add(new MenuFlyoutSeparator());
        Add("Delete", "", async () =>
        {
            await Captures.RecycleAsync(item.Path);
            Reload();
        });
        menu.ShowAt((FrameworkElement)e.OriginalSource, e.GetPosition((FrameworkElement)e.OriginalSource));
    }
}
