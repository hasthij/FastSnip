using System.Numerics;
using FastSnip.App.Services;
using Microsoft.Graphics.Canvas;
using Microsoft.Graphics.Canvas.Effects;
using Microsoft.Graphics.Canvas.Geometry;
using Microsoft.Graphics.Canvas.Text;
using Microsoft.Graphics.Canvas.UI;
using Microsoft.Graphics.Canvas.UI.Xaml;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.Media.Core;
using Windows.Media.Editing;
using Windows.Storage;
using Windows.Storage.Streams;
using Windows.System;
using Windows.UI;
using System.Runtime.InteropServices.WindowsRuntime;
using Colors = Microsoft.UI.Colors;

namespace FastSnip.App.Pages;

/// <summary>
/// Annotate editor (crop, pen, highlighter, arrow, box, text, blur, step
/// numbers) for screenshots, and a trim bar for recordings. Opens only from
/// the toast or the gallery.
/// </summary>
public sealed partial class EditorPage : Page
{
    /// Set before navigating here to open a file.
    public static string? PendingFile { get; set; }
    private static string? _lastFile;

    // ------------------------------------------------------------ model

    private abstract record Ann(Color Color, float Width);
    private sealed record Stroke(Color Color, float Width, List<Vector2> Points, bool Marker) : Ann(Color, Width);
    private sealed record Arrow(Color Color, float Width, Vector2 From, Vector2 To) : Ann(Color, Width);
    private sealed record Box(Color Color, float Width, Rect Area) : Ann(Color, Width);
    private sealed record Label(Color Color, float Width, Vector2 At, string Text, float Size) : Ann(Color, Width);
    private sealed record Blur(Color Color, float Width, Rect Area) : Ann(Color, Width);
    private sealed record Step(Color Color, float Width, Vector2 At, int Number) : Ann(Color, Width);

    private sealed record State(List<Ann> Anns, Rect? Crop);

    private string _path = "";
    private CanvasBitmap? _bitmap;
    private List<Ann> _anns = new();
    private Rect? _crop;
    private Rect? _cropDraft;
    private readonly Stack<State> _undo = new();
    private readonly Stack<State> _redo = new();
    private string _tool = "arrow";
    private Color _color = Look.Hex(0xe5383b);
    private float _width = 4;
    private float _fontSize = 22;
    private Ann? _drawing;
    private Vector2 _start;
    private Vector2? _textAt;
    private bool _dirty;

    // view transform: image -> canvas
    private float _scale = 1;
    private Vector2 _offset;

    private static readonly uint[] SwatchColors = { 0xe5383b, 0xf59f00, 0x1d8a4e, 0x0b7a73, 0x3b5bdb, 0x121a1a, 0xffffff };

    public EditorPage()
    {
        InitializeComponent();
        foreach (var c in SwatchColors)
        {
            var b = new Button
            {
                Width = 22,
                Height = 22,
                Padding = new Thickness(0),
                CornerRadius = new CornerRadius(11),
                Background = new SolidColorBrush(Look.Hex(c)),
                BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
                BorderThickness = new Thickness(c == SwatchColors[0] ? 3 : 1),
                Tag = c,
            };
            ToolTipService.SetToolTip(b, Look.ToHex(Look.Hex(c)));
            b.Click += (s, _) =>
            {
                _color = Look.Hex((uint)((Button)s).Tag);
                foreach (var o in Swatches.Children.OfType<Button>()) o.BorderThickness = new Thickness(o == s ? 3 : 1);
            };
            Swatches.Children.Add(b);
        }
        AddKeys();
    }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        var file = PendingFile ?? _lastFile;
        PendingFile = null;
        if (file == null || !File.Exists(file))
        {
            EmptyState.Visibility = Visibility.Visible;
            return;
        }
        _lastFile = file;
        EmptyState.Visibility = Visibility.Collapsed;
        if (file.EndsWith(".mp4", StringComparison.OrdinalIgnoreCase)) await OpenVideo(file);
        else await OpenImage(file);
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        Player.MediaPlayer?.Pause();
    }

    private void GoCaptures_Click(object sender, RoutedEventArgs e) => App.Window?.Show("captures");

    // ------------------------------------------------------------ open

    private async Task OpenImage(string path)
    {
        ImageEditor.Visibility = Visibility.Visible;
        VideoEditor.Visibility = Visibility.Collapsed;
        _path = path;
        _anns = new();
        _crop = null;
        _undo.Clear();
        _redo.Clear();
        _dirty = false;
        FileName.Text = Path.GetFileName(path);
        SavedText.Text = "";
        // Load into memory so saving over the original is allowed.
        var bytes = await File.ReadAllBytesAsync(path);
        using var stream = new InMemoryRandomAccessStream();
        await stream.WriteAsync(bytes.AsBuffer());
        stream.Seek(0);
        _bitmap = await CanvasBitmap.LoadAsync(CanvasDevice.GetSharedDevice(), stream, 96);
        var sz = _bitmap.SizeInPixels;
        FileInfoText.Text = $"{sz.Width} × {sz.Height}";
        SetTool("arrow");
        Fit();
        Canvas.Invalidate();
    }

    private async Task OpenVideo(string path)
    {
        ImageEditor.Visibility = Visibility.Collapsed;
        VideoEditor.Visibility = Visibility.Visible;
        _path = path;
        VideoName.Text = Path.GetFileName(path);
        var file = await StorageFile.GetFileFromPathAsync(path);
        var props = await file.Properties.GetVideoPropertiesAsync();
        var secs = Math.Max(props.Duration.TotalSeconds, 0.1);
        VideoInfo.Text = $"{props.Width} × {props.Height} · {Fmt(secs)}";
        Player.Source = MediaSource.CreateFromStorageFile(file);
        TrimStart.Maximum = secs;
        TrimEnd.Maximum = secs;
        TrimStart.Value = 0;
        TrimEnd.Value = secs;
        UpdateTrimInfo();
    }

    private void Canvas_CreateResources(CanvasControl sender, CanvasCreateResourcesEventArgs args) { }

    // ------------------------------------------------------------ view

    private void Canvas_SizeChanged(object sender, SizeChangedEventArgs e) => Fit();

    private void Fit()
    {
        if (_bitmap == null) return;
        var (iw, ih) = ((float)_bitmap.Size.Width, (float)_bitmap.Size.Height);
        var (cw, ch) = ((float)Canvas.ActualWidth - 48, (float)Canvas.ActualHeight - 48);
        if (cw <= 0 || ch <= 0) return;
        _scale = Math.Min(Math.Min(cw / iw, ch / ih), 2f);
        _offset = new Vector2(((float)Canvas.ActualWidth - iw * _scale) / 2, ((float)Canvas.ActualHeight - ih * _scale) / 2);
        Canvas.Invalidate();
    }

    private Vector2 ToImage(Point p) => (new Vector2((float)p.X, (float)p.Y) - _offset) / _scale;

    private void Canvas_Draw(CanvasControl sender, CanvasDrawEventArgs args)
    {
        if (_bitmap == null) return;
        var ds = args.DrawingSession;
        var iw = (float)_bitmap.Size.Width;
        var ih = (float)_bitmap.Size.Height;
        // Soft shadow under the image.
        ds.FillRectangle(_offset.X + 2, _offset.Y + 4, iw * _scale, ih * _scale, Color.FromArgb(40, 0, 0, 0));
        ds.Transform = Matrix3x2.CreateScale(_scale) * Matrix3x2.CreateTranslation(_offset);
        DrawScene(ds, _bitmap, includeDraft: true);
        // Dim outside the crop, and the crop being drawn.
        var crop = _tool == "crop" ? _cropDraft ?? _crop : _crop;
        if (crop is Rect c)
        {
            var dim = Color.FromArgb(150, 0, 0, 0);
            ds.FillRectangle(0, 0, iw, (float)c.Y, dim);
            ds.FillRectangle(0, (float)(c.Y + c.Height), iw, ih - (float)(c.Y + c.Height), dim);
            ds.FillRectangle(0, (float)c.Y, (float)c.X, (float)c.Height, dim);
            ds.FillRectangle((float)(c.X + c.Width), (float)c.Y, iw - (float)(c.X + c.Width), (float)c.Height, dim);
            using var dash = new CanvasStrokeStyle { DashStyle = CanvasDashStyle.Dash };
            ds.DrawRectangle(c, Colors.White, 1.5f / _scale, dash);
        }
        ds.Transform = Matrix3x2.Identity;
    }

    private void DrawScene(CanvasDrawingSession ds, CanvasBitmap bmp, bool includeDraft)
    {
        ds.DrawImage(bmp);
        foreach (var a in _anns) DrawAnn(ds, bmp, a);
        if (includeDraft && _drawing != null) DrawAnn(ds, bmp, _drawing);
    }

    private static void DrawAnn(CanvasDrawingSession ds, CanvasBitmap bmp, Ann a)
    {
        var round = new CanvasStrokeStyle { StartCap = CanvasCapStyle.Round, EndCap = CanvasCapStyle.Round, LineJoin = CanvasLineJoin.Round };
        switch (a)
        {
            case Stroke s when s.Points.Count > 0:
                {
                    var color = s.Marker ? Color.FromArgb(110, s.Color.R, s.Color.G, s.Color.B) : s.Color;
                    var w = s.Marker ? s.Width * 4 : s.Width;
                    if (s.Points.Count == 1)
                    {
                        ds.FillCircle(s.Points[0], w / 2, color);
                        break;
                    }
                    using var pb = new CanvasPathBuilder(ds);
                    pb.BeginFigure(s.Points[0]);
                    for (int i = 1; i < s.Points.Count; i++) pb.AddLine(s.Points[i]);
                    pb.EndFigure(CanvasFigureLoop.Open);
                    using var geo = CanvasGeometry.CreatePath(pb);
                    if (s.Marker)
                    {
                        using var layer = ds.CreateLayer(1f);
                        ds.DrawGeometry(geo, color, w, round);
                    }
                    else ds.DrawGeometry(geo, color, w, round);
                    break;
                }
            case Arrow ar:
                {
                    var dir = ar.To - ar.From;
                    var len = dir.Length();
                    if (len < 1) break;
                    dir /= len;
                    var head = Math.Max(ar.Width * 3.2f, 12);
                    var baseP = ar.To - dir * head;
                    var n = new Vector2(-dir.Y, dir.X) * head * 0.6f;
                    ds.DrawLine(ar.From, baseP + dir * 1, ar.Color, ar.Width, round);
                    using var pb = new CanvasPathBuilder(ds);
                    pb.BeginFigure(ar.To);
                    pb.AddLine(baseP + n);
                    pb.AddLine(baseP - n);
                    pb.EndFigure(CanvasFigureLoop.Closed);
                    using var geo = CanvasGeometry.CreatePath(pb);
                    ds.FillGeometry(geo, ar.Color);
                    break;
                }
            case Box b:
                ds.DrawRoundedRectangle(b.Area, 4, 4, b.Color, b.Width, round);
                break;
            case Blur bl when bl.Area.Width > 1 && bl.Area.Height > 1:
                {
                    using var layer = ds.CreateLayer(1f, bl.Area);
                    var blur = new GaussianBlurEffect { Source = bmp, BlurAmount = 14, BorderMode = EffectBorderMode.Hard, Optimization = EffectOptimization.Speed };
                    ds.DrawImage(blur);
                    break;
                }
            case Label l when l.Text.Length > 0:
                {
                    using var fmt = new CanvasTextFormat { FontSize = l.Size, FontWeight = Microsoft.UI.Text.FontWeights.Bold, FontFamily = "Segoe UI Variable Display" };
                    using var layout = new CanvasTextLayout(ds, l.Text, fmt, 2000, 2000);
                    var r = layout.LayoutBounds;
                    var pad = l.Size * 0.25f;
                    var bg = Luma(l.Color) > 0.6 ? Color.FromArgb(230, 20, 26, 26) : Color.FromArgb(235, 255, 255, 255);
                    ds.FillRoundedRectangle(l.At.X - pad, l.At.Y - pad * 0.6f, (float)r.Width + pad * 2, (float)r.Height + pad * 1.2f, 4, 4, bg);
                    ds.DrawTextLayout(layout, l.At, l.Color);
                    break;
                }
            case Step st:
                {
                    var r = Math.Max(st.Width * 3, 13);
                    ds.FillCircle(st.At, r, st.Color);
                    using var fmt = new CanvasTextFormat
                    {
                        FontSize = r * 1.1f,
                        FontWeight = Microsoft.UI.Text.FontWeights.Bold,
                        HorizontalAlignment = CanvasHorizontalAlignment.Center,
                        VerticalAlignment = CanvasVerticalAlignment.Center,
                    };
                    var fg = Luma(st.Color) > 0.6 ? Colors.Black : Colors.White;
                    ds.DrawText(st.Number.ToString(), new Rect(st.At.X - r, st.At.Y - r, r * 2, r * 2), fg, fmt);
                    break;
                }
        }
    }

    private static double Luma(Color c) => (0.2126 * c.R + 0.7152 * c.G + 0.0722 * c.B) / 255;

    // ------------------------------------------------------------ tools

    private void Tool_Click(object sender, RoutedEventArgs e) => SetTool((string)((FrameworkElement)sender).Tag);

    private void SetTool(string tool)
    {
        CommitText();
        _tool = tool;
        foreach (var b in Rail.Children.OfType<ToggleButton>()) b.IsChecked = (string)b.Tag == tool;
        _cropDraft = tool == "crop" ? _crop : null;
        ApplyCrop.Visibility = Visibility.Collapsed;
        Hint.Text = tool switch
        {
            "crop" => "Drag to choose the area to keep, then Apply crop",
            "pen" => "Draw freely",
            "highlighter" => "Drag over text to highlight it",
            "arrow" => "Drag to draw · Shift snaps to 45°",
            "box" => "Drag to draw a box",
            "text" => "Click where the text goes",
            "blur" => "Drag over anything private",
            "step" => "Click to place the next number",
            _ => "",
        };
        Canvas.Invalidate();
    }

    private void Push()
    {
        _undo.Push(new State(new List<Ann>(_anns), _crop));
        _redo.Clear();
        _dirty = true;
    }

    private void Undo_Click(object sender, RoutedEventArgs e) => Undo();
    private void Redo_Click(object sender, RoutedEventArgs e) => Redo();

    private void Undo()
    {
        if (_undo.Count == 0) return;
        _redo.Push(new State(new List<Ann>(_anns), _crop));
        var s = _undo.Pop();
        (_anns, _crop) = (s.Anns, s.Crop);
        Canvas.Invalidate();
    }

    private void Redo()
    {
        if (_redo.Count == 0) return;
        _undo.Push(new State(new List<Ann>(_anns), _crop));
        var s = _redo.Pop();
        (_anns, _crop) = (s.Anns, s.Crop);
        Canvas.Invalidate();
    }

    private static Rect RectFrom(Vector2 a, Vector2 b) =>
        new(Math.Min(a.X, b.X), Math.Min(a.Y, b.Y), Math.Abs(a.X - b.X), Math.Abs(a.Y - b.Y));

    private Vector2 Clamp(Vector2 p) => _bitmap == null ? p :
        Vector2.Clamp(p, Vector2.Zero, new Vector2((float)_bitmap.Size.Width, (float)_bitmap.Size.Height));

    private void Canvas_PointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (_bitmap == null) return;
        var pt = e.GetCurrentPoint(Canvas);
        if (!pt.Properties.IsLeftButtonPressed) return;
        CommitText();
        Canvas.CapturePointer(e.Pointer);
        var p = Clamp(ToImage(pt.Position));
        _start = p;
        switch (_tool)
        {
            case "pen": _drawing = new Stroke(_color, _width, new List<Vector2> { p }, false); break;
            case "highlighter": _drawing = new Stroke(_color, _width, new List<Vector2> { p }, true); break;
            case "arrow": _drawing = new Arrow(_color, _width, p, p); break;
            case "box": _drawing = new Box(_color, _width, new Rect(p.X, p.Y, 0, 0)); break;
            case "blur": _drawing = new Blur(_color, _width, new Rect(p.X, p.Y, 0, 0)); break;
            case "crop": _cropDraft = new Rect(p.X, p.Y, 0, 0); break;
            case "step":
                Push();
                _anns.Add(new Step(_color, _width, p, _anns.OfType<Step>().Count() + 1));
                break;
            case "text":
                _textAt = p;
                Microsoft.UI.Xaml.Controls.Canvas.SetLeft(TextEntry, p.X * _scale + _offset.X);
                Microsoft.UI.Xaml.Controls.Canvas.SetTop(TextEntry, p.Y * _scale + _offset.Y - 6);
                TextEntry.Text = "";
                TextEntry.Visibility = Visibility.Visible;
                TextEntry.Focus(FocusState.Programmatic);
                break;
        }
        Canvas.Invalidate();
    }

    private void Canvas_PointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (_bitmap == null) return;
        var pt = e.GetCurrentPoint(Canvas);
        if (!pt.Properties.IsLeftButtonPressed) return;
        var p = Clamp(ToImage(pt.Position));
        var shift = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Shift).HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);
        switch (_drawing)
        {
            case Stroke s:
                if (s.Points.Count == 0 || Vector2.Distance(s.Points[^1], p) > 1.5f / _scale) s.Points.Add(p);
                break;
            case Arrow a:
                if (shift)
                {
                    var d = p - a.From;
                    var ang = Math.Round(Math.Atan2(d.Y, d.X) / (Math.PI / 4)) * (Math.PI / 4);
                    p = a.From + new Vector2((float)Math.Cos(ang), (float)Math.Sin(ang)) * d.Length();
                }
                _drawing = a with { To = p };
                break;
            case Box b: _drawing = b with { Area = RectFrom(_start, p) }; break;
            case Blur b: _drawing = b with { Area = RectFrom(_start, p) }; break;
        }
        if (_tool == "crop" && _cropDraft != null) _cropDraft = RectFrom(_start, p);
        Canvas.Invalidate();
    }

    private void Canvas_PointerReleased(object sender, PointerRoutedEventArgs e)
    {
        Canvas.ReleasePointerCapture(e.Pointer);
        if (_drawing != null)
        {
            var keep = _drawing switch
            {
                Arrow a => Vector2.Distance(a.From, a.To) > 4,
                Box b => b.Area.Width > 3 && b.Area.Height > 3,
                Blur b => b.Area.Width > 3 && b.Area.Height > 3,
                _ => true,
            };
            if (keep)
            {
                Push();
                _anns.Add(_drawing);
            }
            _drawing = null;
        }
        if (_tool == "crop" && _cropDraft is Rect r && r.Width > 8 && r.Height > 8) ApplyCrop.Visibility = Visibility.Visible;
        Canvas.Invalidate();
    }

    private void ApplyCrop_Click(object sender, RoutedEventArgs e)
    {
        if (_cropDraft is not Rect r) return;
        Push();
        _crop = new Rect(Math.Round(r.X), Math.Round(r.Y), Math.Round(r.Width), Math.Round(r.Height));
        ApplyCrop.Visibility = Visibility.Collapsed;
        SetTool("arrow");
    }

    private void CommitText()
    {
        if (TextEntry.Visibility != Visibility.Visible) return;
        TextEntry.Visibility = Visibility.Collapsed;
        if (_textAt is Vector2 at && TextEntry.Text.Trim().Length > 0)
        {
            Push();
            _anns.Add(new Label(_color, _width, at, TextEntry.Text.Trim(), _fontSize));
        }
        _textAt = null;
        Canvas.Invalidate();
    }

    private void TextEntry_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter) { CommitText(); e.Handled = true; }
        if (e.Key == VirtualKey.Escape) { TextEntry.Text = ""; CommitText(); e.Handled = true; }
    }

    private void TextEntry_LostFocus(object sender, RoutedEventArgs e) => CommitText();

    private void WidthSlider_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        _width = (float)e.NewValue;
        if (WidthLabel != null) WidthLabel.Text = $"Thickness · {(int)e.NewValue} px";
    }

    private void FontSlider_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        _fontSize = (float)e.NewValue;
        if (FontLabel != null) FontLabel.Text = $"Text size · {(int)e.NewValue} px";
    }

    // ------------------------------------------------------------ output

    private async Task<IRandomAccessStream?> RenderPng()
    {
        if (_bitmap == null) return null;
        CommitText();
        var area = _crop ?? new Rect(0, 0, _bitmap.Size.Width, _bitmap.Size.Height);
        var device = CanvasDevice.GetSharedDevice();
        using var target = new CanvasRenderTarget(device, (float)area.Width, (float)area.Height, 96);
        using (var ds = target.CreateDrawingSession())
        {
            ds.Clear(Colors.Transparent);
            ds.Transform = Matrix3x2.CreateTranslation((float)-area.X, (float)-area.Y);
            DrawScene(ds, _bitmap, includeDraft: false);
        }
        var stream = new InMemoryRandomAccessStream();
        await target.SaveAsync(stream, CanvasBitmapFileFormat.Png);
        stream.Seek(0);
        return stream;
    }

    private async void Copy_Click(object sender, RoutedEventArgs e) => await CopyImage();

    private async Task CopyImage()
    {
        var stream = await RenderPng();
        if (stream == null) return;
        var dp = new DataPackage();
        dp.SetBitmap(RandomAccessStreamReference.CreateFromStream(stream));
        Clipboard.SetContent(dp);
        Clipboard.Flush();
        SavedText.Text = "Copied to the clipboard";
    }

    private async Task SaveTo(string path)
    {
        using var stream = await RenderPng();
        if (stream == null) return;
        var tmp = path + ".tmp";
        using (var fs = File.Create(tmp))
        {
            await stream.AsStreamForRead().CopyToAsync(fs);
        }
        File.Move(tmp, path, overwrite: true);
        _dirty = false;
        SavedText.Text = $"Saved {Path.GetFileName(path)}";
    }

    private async void Save_Click(object sender, RoutedEventArgs e) => await SaveTo(_path);

    private async void SaveCopy_Click(object sender, RoutedEventArgs e) => await SaveCopy();

    private async Task SaveCopy()
    {
        var dir = Path.GetDirectoryName(_path)!;
        var stem = Path.GetFileNameWithoutExtension(_path);
        var target = Path.Combine(dir, $"{stem} (edited).png");
        for (int i = 2; File.Exists(target); i++) target = Path.Combine(dir, $"{stem} (edited {i}).png");
        await SaveTo(target);
    }

    // ------------------------------------------------------------ keys

    private void AddKeys()
    {
        void Key(VirtualKey k, VirtualKeyModifiers m, Func<Task> act)
        {
            var ka = new KeyboardAccelerator { Key = k, Modifiers = m };
            ka.Invoked += async (_, e) =>
            {
                if (TextEntry.Visibility == Visibility.Visible || ImageEditor.Visibility != Visibility.Visible) return;
                e.Handled = true;
                await act();
            };
            KeyboardAccelerators.Add(ka);
        }
        Task Sync(Action a) { a(); return Task.CompletedTask; }
        Key(VirtualKey.Z, VirtualKeyModifiers.Control, () => Sync(Undo));
        Key(VirtualKey.Y, VirtualKeyModifiers.Control, () => Sync(Redo));
        Key(VirtualKey.C, VirtualKeyModifiers.Control, CopyImage);
        Key(VirtualKey.S, VirtualKeyModifiers.Control, () => SaveTo(_path));
        Key(VirtualKey.S, VirtualKeyModifiers.Control | VirtualKeyModifiers.Shift, SaveCopy);
        foreach (var (k, t) in new[] { (VirtualKey.C, "crop"), (VirtualKey.P, "pen"), (VirtualKey.H, "highlighter"), (VirtualKey.A, "arrow"),
                     (VirtualKey.B, "box"), (VirtualKey.T, "text"), (VirtualKey.X, "blur"), (VirtualKey.N, "step") })
            Key(k, VirtualKeyModifiers.None, () => Sync(() => SetTool(t)));
        Key(VirtualKey.Enter, VirtualKeyModifiers.None, () => Sync(() => { if (ApplyCrop.Visibility == Visibility.Visible) ApplyCrop_Click(this, new RoutedEventArgs()); }));
    }

    // ------------------------------------------------------------ video trim

    private static string Fmt(double s) => TimeSpan.FromSeconds(s).ToString(s >= 3600 ? @"h\:mm\:ss\.f" : @"m\:ss\.f");

    private void Trim_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        if (TrimStart == null || TrimEnd == null) return;
        if (sender == TrimStart && TrimStart.Value > TrimEnd.Value - 0.2) TrimStart.Value = Math.Max(0, TrimEnd.Value - 0.2);
        if (sender == TrimEnd && TrimEnd.Value < TrimStart.Value + 0.2) TrimEnd.Value = Math.Min(TrimEnd.Maximum, TrimStart.Value + 0.2);
        if (Player.MediaPlayer != null)
            Player.MediaPlayer.PlaybackSession.Position = TimeSpan.FromSeconds(sender == TrimEnd ? TrimEnd.Value : TrimStart.Value);
        UpdateTrimInfo();
    }

    private void UpdateTrimInfo()
    {
        var keep = TrimEnd.Value - TrimStart.Value;
        TrimInfo.Text = $"{Fmt(TrimStart.Value)} → {Fmt(TrimEnd.Value)} · keeps {keep:0.0} s of {TrimEnd.Maximum:0.0} s";
    }

    private async void SaveTrim_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            SaveTrim.IsEnabled = false;
            TrimProgress.Visibility = Visibility.Visible;
            TrimProgress.Value = 0;
            Player.MediaPlayer?.Pause();
            var file = await StorageFile.GetFileFromPathAsync(_path);
            var clip = await MediaClip.CreateFromFileAsync(file);
            clip.TrimTimeFromStart = TimeSpan.FromSeconds(TrimStart.Value);
            clip.TrimTimeFromEnd = TimeSpan.FromSeconds(Math.Max(0, TrimEnd.Maximum - TrimEnd.Value));
            var comp = new MediaComposition();
            comp.Clips.Add(clip);
            var folder = await file.GetParentAsync();
            var name = Path.GetFileNameWithoutExtension(_path) + " (trimmed).mp4";
            var outFile = await folder.CreateFileAsync(name, CreationCollisionOption.GenerateUniqueName);
            var op = comp.RenderToFileAsync(outFile, SnapKeyframes.IsOn ? MediaTrimmingPreference.Fast : MediaTrimmingPreference.Precise);
            op.Progress = (_, p) => DispatcherQueue.TryEnqueue(() => TrimProgress.Value = p);
            var result = await op;
            TrimInfo.Text = result == Windows.Media.Transcoding.TranscodeFailureReason.None
                ? $"Saved {outFile.Name}"
                : $"Couldn't save the trimmed video ({result}).";
        }
        catch (Exception ex)
        {
            TrimInfo.Text = $"Couldn't save the trimmed video: {ex.Message}";
        }
        finally
        {
            SaveTrim.IsEnabled = true;
            TrimProgress.Visibility = Visibility.Collapsed;
        }
    }
}
