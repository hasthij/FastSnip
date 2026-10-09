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

    public CapturesPage()
    {
        InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        Reload();
        Watch();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        _shots?.Dispose();
        _videos?.Dispose();
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

    private void Reload()
    {
        _all = Captures.List(App.Settings);
        Apply();
        foreach (var item in _all.Take(200)) _ = LoadThumb(item);
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
        Add("Show in folder", "", () =>
        {
            System.Diagnostics.Process.Start("explorer.exe", $"/select,\"{item.Path}\"");
            return Task.CompletedTask;
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
