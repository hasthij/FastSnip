using System.Numerics;
using System.Runtime.CompilerServices;
using FastSnip.App.Services;
using Microsoft.Graphics.Canvas;
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
/// Screenshot editor: select/move/resize annotations, crop, pen, highlighter,
/// arrow, line, box, ellipse, text, step numbers, pixelate; zoom and pan.
/// Also a trim bar for recordings. Opens only from the toast or the gallery.
/// </summary>
public sealed partial class EditorPage : Page
{
    /// Set before navigating here to open a file.
    public static string? PendingFile { get; set; }
    private static string? _lastFile;

    // ------------------------------------------------------------ model (immutable; edits use `with`)

    private abstract record Ann(Color Color, float Width);
    private sealed record Stroke(Color Color, float Width, List<Vector2> Points, bool Marker) : Ann(Color, Width);
    private sealed record Seg(Color Color, float Width, Vector2 From, Vector2 To, bool Head) : Ann(Color, Width);
    private sealed record Shape(Color Color, float Width, Rect Area, bool Oval) : Ann(Color, Width);
    private sealed record Label(Color Color, float Width, Vector2 At, string Text, float Size) : Ann(Color, Width);
    private sealed record Pixel(Color Color, float Width, Rect Area) : Ann(Color, Width);
    private sealed record Step(Color Color, float Width, Vector2 At, int Number) : Ann(Color, Width);

    private sealed record State(List<Ann> Anns, Rect? Crop);

    private enum Mode { None, Draw, Move, Resize, Pan, CropNew, CropResize, CropMove }

    private string _path = "";
    private CanvasBitmap? _bitmap;
    private List<Ann> _anns = new();
    private Rect? _crop;
    private Rect? _cropDraft;
    private readonly Stack<State> _undo = new();
    private readonly Stack<State> _redo = new();
    private string _tool = "select";
    private Color _color = Look.Hex(0xe5383b);
    private float _width = 4;
    private float _font = 22;
    private Ann? _draft;
    private int _sel = -1;
    private Mode _mode;
    private Vector2 _start, _startRaw, _panStart, _panOff;
    private Ann? _orig;
    private Rect _origRect;
    private int _handle;
    private bool _moved;
    private bool _syncing, _stylePushed;
    private int _editIdx = -1;
    private Vector2? _textAt;
    private Color _accent = Color.FromArgb(255, 76, 141, 255);

    // view transform: image -> surface (DIPs)
    private float _scale = 1;
    private Vector2 _offset;
    private bool _fitted = true;

    private static readonly uint[] SwatchColors = { 0xe5383b, 0xf59f00, 0xffd43b, 0x2f9e44, 0x3b5bdb, 0x7048e8, 0x121212, 0xffffff };

    private static readonly CanvasStrokeStyle Round = new() { StartCap = CanvasCapStyle.Round, EndCap = CanvasCapStyle.Round, LineJoin = CanvasLineJoin.Round };
    private static readonly ConditionalWeakTable<Stroke, CanvasGeometry> GeoCache = new();
    private static readonly ConditionalWeakTable<Label, CanvasTextLayout> LayoutCache = new();
    private static readonly ConditionalWeakTable<Pixel, CanvasRenderTarget> PixelCache = new();

    public EditorPage()
    {
        InitializeComponent();
        NavigationCacheMode = NavigationCacheMode.Required;
        try { _accent = (Color)Application.Current.Resources["SystemAccentColor"]; } catch { }
        foreach (var c in SwatchColors)
        {
            var b = new Button
            {
                Width = 24,
                Height = 24,
                MinWidth = 0,
                MinHeight = 0,
                Padding = new Thickness(0),
                CornerRadius = new CornerRadius(12),
                Background = new SolidColorBrush(Look.Hex(c)),
                BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
                BorderThickness = new Thickness(1),
                Tag = c,
            };
            ToolTipService.SetToolTip(b, $"#{c:x6}");
            b.Click += (s, _) =>
            {
                _color = Look.Hex((uint)((Button)s).Tag);
                _stylePushed = false;
                ApplyStyle();
                _stylePushed = false;
                HighlightSwatch();
            };
            Swatches.Children.Add(b);
        }
        HighlightSwatch();
        WidthSlider.AddHandler(UIElement.PointerReleasedEvent, new PointerEventHandler((_, _) => _stylePushed = false), true);
        FontSlider.AddHandler(UIElement.PointerReleasedEvent, new PointerEventHandler((_, _) => _stylePushed = false), true);
        AddKeys();
    }

    protected override async void OnNavigatedTo(NavigationEventArgs e)
    {
        var file = PendingFile ?? _lastFile;
        PendingFile = null;
        // Same file still open (the page is cached): nothing to reload.
        if (file != null && file == _path && (ImageEditor.Visibility == Visibility.Visible || VideoEditor.Visibility == Visibility.Visible)) return;
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
        _cropDraft = null;
        _draft = null;
        _sel = -1;
        _mode = Mode.None;
        _undo.Clear();
        _redo.Clear();
        TextEntry.Visibility = Visibility.Collapsed;
        _textAt = null;
        _editIdx = -1;
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
        _fitted = true;
        SetTool("select");
        Fit();
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

    // ------------------------------------------------------------ view

    private float Rs => (float)(Surface.XamlRoot?.RasterizationScale ?? 1.0);
    private Vector2 ToImage(Point p) => (new Vector2((float)p.X, (float)p.Y) - _offset) / _scale;
    private Vector2 ToScreen(Vector2 v) => v * _scale + _offset;
    private static Vector2 V(Point p) => new((float)p.X, (float)p.Y);
    private Rect Full => new(0, 0, _bitmap!.Size.Width, _bitmap.Size.Height);

    private void Surface_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_fitted) Fit();
        else Surface.Invalidate();
    }

    private void Fit()
    {
        if (_bitmap == null) return;
        var (iw, ih) = ((float)_bitmap.Size.Width, (float)_bitmap.Size.Height);
        var (cw, ch) = ((float)Surface.ActualWidth - 48, (float)Surface.ActualHeight - 48);
        if (cw <= 0 || ch <= 0) return;
        _fitted = true;
        _scale = Math.Min(Math.Min(cw / iw, ch / ih), 2f / Rs);
        _offset = new Vector2(((float)Surface.ActualWidth - iw * _scale) / 2, ((float)Surface.ActualHeight - ih * _scale) / 2);
        Redraw();
    }

    private void SetZoom(float ns, Vector2 anchor)
    {
        ns = Math.Clamp(ns, 0.05f, 16f);
        _offset = anchor - (anchor - _offset) * (ns / _scale);
        _scale = ns;
        _fitted = false;
        Redraw();
    }

    private Vector2 Center => new((float)Surface.ActualWidth / 2, (float)Surface.ActualHeight / 2);

    private void Redraw()
    {
        ZoomText.Text = $"{_scale * Rs * 100:0}%";
        Surface.Invalidate();
    }

    private void ZoomOut_Click(object sender, RoutedEventArgs e) { if (_bitmap != null) SetZoom(_scale / 1.25f, Center); }
    private void ZoomIn_Click(object sender, RoutedEventArgs e) { if (_bitmap != null) SetZoom(_scale * 1.25f, Center); }
    private void ZoomFit_Click(object sender, RoutedEventArgs e) => Fit();
    private void ZoomActual_Click(object sender, RoutedEventArgs e) { if (_bitmap != null) SetZoom(1f / Rs, Center); }

    private void Surface_Wheel(object sender, PointerRoutedEventArgs e)
    {
        if (_bitmap == null) return;
        var pt = e.GetCurrentPoint(Surface);
        SetZoom(_scale * (float)Math.Pow(1.1, pt.Properties.MouseWheelDelta / 120.0), V(pt.Position));
        e.Handled = true;
    }

    // ------------------------------------------------------------ drawing

    private void Surface_Draw(CanvasControl sender, CanvasDrawEventArgs args)
    {
        if (_bitmap == null) return;
        var ds = args.DrawingSession;
        var iw = (float)_bitmap.Size.Width;
        var ih = (float)_bitmap.Size.Height;
        ds.FillRectangle(_offset.X + 2, _offset.Y + 4, iw * _scale, ih * _scale, Color.FromArgb(70, 0, 0, 0));
        ds.Transform = Matrix3x2.CreateScale(_scale) * Matrix3x2.CreateTranslation(_offset);
        var interp = _scale * Rs >= 2 ? CanvasImageInterpolation.NearestNeighbor : CanvasImageInterpolation.Linear;
        ds.DrawImage(_bitmap, new Rect(0, 0, iw, ih), new Rect(0, 0, iw, ih), 1f, interp);
        DrawAnns(ds, _bitmap, view: true);
        ds.Transform = Matrix3x2.Identity;

        // Crop: dim what will be removed.
        var crop = _tool == "crop" ? _cropDraft : _crop;
        if (crop is Rect c)
        {
            var tl = ToScreen(new Vector2((float)c.X, (float)c.Y));
            var br = ToScreen(new Vector2((float)(c.X + c.Width), (float)(c.Y + c.Height)));
            var il = _offset.X; var it = _offset.Y; var ir = _offset.X + iw * _scale; var ib = _offset.Y + ih * _scale;
            var dim = Color.FromArgb(150, 0, 0, 0);
            ds.FillRectangle(il, it, ir - il, tl.Y - it, dim);
            ds.FillRectangle(il, br.Y, ir - il, ib - br.Y, dim);
            ds.FillRectangle(il, tl.Y, tl.X - il, br.Y - tl.Y, dim);
            ds.FillRectangle(br.X, tl.Y, ir - br.X, br.Y - tl.Y, dim);
            if (_tool == "crop")
            {
                var line = Color.FromArgb(120, 255, 255, 255);
                for (int i = 1; i <= 2; i++)
                {
                    var x = tl.X + (br.X - tl.X) * i / 3;
                    var y = tl.Y + (br.Y - tl.Y) * i / 3;
                    ds.DrawLine(x, tl.Y, x, br.Y, line, 1);
                    ds.DrawLine(tl.X, y, br.X, y, line, 1);
                }
                ds.DrawRectangle(tl.X, tl.Y, br.X - tl.X, br.Y - tl.Y, Colors.White, 1.5f);
                foreach (var h in HandlePts(c)) DrawHandle(ds, ToScreen(h));
            }
        }

        // Selection.
        if (_tool == "select" && _sel >= 0 && _sel < _anns.Count)
        {
            var a = _anns[_sel];
            if (a is not Seg)
            {
                var b = Bounds(a);
                var tl = ToScreen(new Vector2((float)b.X, (float)b.Y));
                var br = ToScreen(new Vector2((float)(b.X + b.Width), (float)(b.Y + b.Height)));
                ds.DrawRectangle(tl.X - 2, tl.Y - 2, br.X - tl.X + 4, br.Y - tl.Y + 4, _accent, 1.5f);
            }
            foreach (var h in Handles(a)) DrawHandle(ds, ToScreen(h));
        }
    }

    private void DrawHandle(CanvasDrawingSession ds, Vector2 p)
    {
        ds.FillRectangle(p.X - 4, p.Y - 4, 8, 8, Colors.White);
        ds.DrawRectangle(p.X - 4, p.Y - 4, 8, 8, _accent, 1.5f);
    }

    /// Annotations in image space. Shared by the view and the export.
    private void DrawAnns(CanvasDrawingSession ds, CanvasBitmap bmp, bool view)
    {
        for (int i = 0; i < _anns.Count; i++)
        {
            if (view && i == _editIdx) continue;
            DrawAnn(ds, bmp, _anns[i], draft: false);
        }
        if (view && _draft != null) DrawAnn(ds, bmp, _draft, draft: true);
    }

    private static CanvasGeometry StrokeGeo(ICanvasResourceCreator rc, Stroke s)
    {
        var pts = s.Points;
        using var pb = new CanvasPathBuilder(rc);
        pb.BeginFigure(pts[0]);
        if (pts.Count == 2) pb.AddLine(pts[1]);
        else
        {
            for (int i = 1; i < pts.Count - 1; i++) pb.AddQuadraticBezier(pts[i], (pts[i] + pts[i + 1]) / 2);
            pb.AddLine(pts[^1]);
        }
        pb.EndFigure(CanvasFigureLoop.Open);
        return CanvasGeometry.CreatePath(pb);
    }

    private static CanvasTextLayout LayoutOf(Label l)
    {
        if (LayoutCache.TryGetValue(l, out var cached)) return cached;
        using var fmt = new CanvasTextFormat { FontSize = l.Size, FontWeight = Microsoft.UI.Text.FontWeights.Bold, FontFamily = "Segoe UI Variable Display" };
        var layout = new CanvasTextLayout(CanvasDevice.GetSharedDevice(), l.Text, fmt, 4000, 4000);
        LayoutCache.Add(l, layout);
        return layout;
    }

    private static Rect LabelBox(Label l)
    {
        var r = LayoutOf(l).LayoutBounds;
        var pad = l.Size * 0.25f;
        return new Rect(l.At.X - pad, l.At.Y - pad * 0.6, r.Width + pad * 2, r.Height + pad * 1.2);
    }

    private static void DrawAnn(CanvasDrawingSession ds, CanvasBitmap bmp, Ann a, bool draft)
    {
        switch (a)
        {
            case Stroke s when s.Points.Count > 0:
                {
                    var w = s.Marker ? s.Width * 4 : s.Width;
                    if (s.Points.Count == 1) { ds.FillCircle(s.Points[0], w / 2, s.Color); break; }
                    CanvasGeometry? geo;
                    if (draft) geo = StrokeGeo(ds, s);
                    else if (!GeoCache.TryGetValue(s, out geo)) { geo = StrokeGeo(CanvasDevice.GetSharedDevice(), s); GeoCache.Add(s, geo); }
                    if (s.Marker)
                    {
                        using var layer = ds.CreateLayer(0.4f);
                        ds.DrawGeometry(geo, s.Color, w, Round);
                    }
                    else ds.DrawGeometry(geo, s.Color, w, Round);
                    if (draft) geo.Dispose();
                    break;
                }
            case Seg g:
                {
                    var dir = g.To - g.From;
                    var len = dir.Length();
                    if (len < 1) break;
                    dir /= len;
                    if (!g.Head) { ds.DrawLine(g.From, g.To, g.Color, g.Width, Round); break; }
                    var head = Math.Max(g.Width * 3.2f, 12);
                    var baseP = g.To - dir * head;
                    var n = new Vector2(-dir.Y, dir.X) * head * 0.6f;
                    ds.DrawLine(g.From, baseP + dir, g.Color, g.Width, Round);
                    using var pb = new CanvasPathBuilder(ds);
                    pb.BeginFigure(g.To);
                    pb.AddLine(baseP + n);
                    pb.AddLine(baseP - n);
                    pb.EndFigure(CanvasFigureLoop.Closed);
                    using var geo = CanvasGeometry.CreatePath(pb);
                    ds.FillGeometry(geo, g.Color);
                    break;
                }
            case Shape h when h.Area.Width > 0 && h.Area.Height > 0:
                if (h.Oval) ds.DrawEllipse((float)(h.Area.X + h.Area.Width / 2), (float)(h.Area.Y + h.Area.Height / 2), (float)h.Area.Width / 2, (float)h.Area.Height / 2, h.Color, h.Width, Round);
                else ds.DrawRectangle(h.Area, h.Color, h.Width, Round);
                break;
            case Pixel px when px.Area.Width > 1 && px.Area.Height > 1:
                {
                    var blk = Math.Max(6f, (float)Math.Max(bmp.Size.Width, bmp.Size.Height) / 120f);
                    var w = Math.Max(1, (int)Math.Ceiling(px.Area.Width / blk));
                    var hh = Math.Max(1, (int)Math.Ceiling(px.Area.Height / blk));
                    CanvasRenderTarget Make()
                    {
                        var rt = new CanvasRenderTarget(CanvasDevice.GetSharedDevice(), w, hh, 96);
                        using var sds = rt.CreateDrawingSession();
                        sds.DrawImage(bmp, new Rect(0, 0, w, hh), px.Area, 1f, CanvasImageInterpolation.HighQualityCubic);
                        return rt;
                    }
                    CanvasRenderTarget? small;
                    if (draft) small = Make();
                    else if (!PixelCache.TryGetValue(px, out small)) { small = Make(); PixelCache.Add(px, small); }
                    using (var layer = ds.CreateLayer(1f, px.Area))
                        ds.DrawImage(small, new Rect(px.Area.X, px.Area.Y, w * blk, hh * blk), new Rect(0, 0, w, hh), 1f, CanvasImageInterpolation.NearestNeighbor);
                    if (draft) small.Dispose();
                    break;
                }
            case Label l when l.Text.Length > 0:
                {
                    var box = LabelBox(l);
                    var bg = Luma(l.Color) > 0.6 ? Color.FromArgb(230, 20, 26, 26) : Color.FromArgb(235, 255, 255, 255);
                    ds.FillRoundedRectangle(box, 4, 4, bg);
                    ds.DrawTextLayout(LayoutOf(l), l.At, l.Color);
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

    // ------------------------------------------------------------ geometry helpers

    private static Rect RectFrom(Vector2 a, Vector2 b) =>
        new(Math.Min(a.X, b.X), Math.Min(a.Y, b.Y), Math.Abs(a.X - b.X), Math.Abs(a.Y - b.Y));

    private Vector2 Clamp(Vector2 p) => _bitmap == null ? p :
        Vector2.Clamp(p, Vector2.Zero, new Vector2((float)_bitmap.Size.Width, (float)_bitmap.Size.Height));

    // TL, T, TR, R, BR, B, BL, L
    private static Vector2[] HandlePts(Rect r)
    {
        float l = (float)r.X, t = (float)r.Y, rr = (float)(r.X + r.Width), b = (float)(r.Y + r.Height), mx = (l + rr) / 2, my = (t + b) / 2;
        return new[] { new Vector2(l, t), new Vector2(mx, t), new Vector2(rr, t), new Vector2(rr, my), new Vector2(rr, b), new Vector2(mx, b), new Vector2(l, b), new Vector2(l, my) };
    }

    private static Rect ResizeRect(Rect r, int idx, Vector2 p)
    {
        double l = r.X, t = r.Y, rr = r.X + r.Width, b = r.Y + r.Height;
        if (idx is 0 or 6 or 7) l = p.X;
        if (idx is 2 or 3 or 4) rr = p.X;
        if (idx is 0 or 1 or 2) t = p.Y;
        if (idx is 4 or 5 or 6) b = p.Y;
        return new Rect(Math.Min(l, rr), Math.Min(t, b), Math.Abs(rr - l), Math.Abs(b - t));
    }

    private static Vector2[] Handles(Ann a) => a switch
    {
        Shape s => HandlePts(s.Area),
        Pixel p => HandlePts(p.Area),
        Seg g => new[] { g.From, g.To },
        _ => Array.Empty<Vector2>(),
    };

    private int HitHandle(Vector2[] pts, Vector2 screen)
    {
        for (int i = 0; i < pts.Length; i++)
            if (Vector2.Distance(ToScreen(pts[i]), screen) <= 9) return i;
        return -1;
    }

    private static Rect Bounds(Ann a)
    {
        switch (a)
        {
            case Stroke s when s.Points.Count > 0:
                {
                    var min = s.Points[0]; var max = min;
                    foreach (var p in s.Points) { min = Vector2.Min(min, p); max = Vector2.Max(max, p); }
                    var pad = (s.Marker ? s.Width * 4 : s.Width) / 2;
                    return new Rect(min.X - pad, min.Y - pad, max.X - min.X + pad * 2, max.Y - min.Y + pad * 2);
                }
            case Seg g:
                {
                    var pad = g.Width / 2 + (g.Head ? Math.Max(g.Width * 3.2f, 12) : 0);
                    var r = RectFrom(g.From, g.To);
                    return new Rect(r.X - pad, r.Y - pad, r.Width + pad * 2, r.Height + pad * 2);
                }
            case Shape h: return new Rect(h.Area.X - h.Width / 2, h.Area.Y - h.Width / 2, h.Area.Width + h.Width, h.Area.Height + h.Width);
            case Pixel p: return p.Area;
            case Label l: return LabelBox(l);
            case Step st: { var r = Math.Max(st.Width * 3, 13); return new Rect(st.At.X - r, st.At.Y - r, r * 2, r * 2); }
        }
        return new Rect();
    }

    private static float DistSeg(Vector2 p, Vector2 a, Vector2 b)
    {
        var ab = b - a;
        var l2 = ab.LengthSquared();
        var t = l2 < 1e-6f ? 0 : Math.Clamp(Vector2.Dot(p - a, ab) / l2, 0, 1);
        return Vector2.Distance(p, a + ab * t);
    }

    private static bool Hit(Ann a, Vector2 p, float tol)
    {
        switch (a)
        {
            case Stroke s:
                {
                    var r = tol + (s.Marker ? s.Width * 4 : s.Width) / 2;
                    if (s.Points.Count == 1) return Vector2.Distance(p, s.Points[0]) <= r;
                    for (int i = 1; i < s.Points.Count; i++) if (DistSeg(p, s.Points[i - 1], s.Points[i]) <= r) return true;
                    return false;
                }
            case Seg g: return DistSeg(p, g.From, g.To) <= tol + g.Width / 2 + (g.Head ? 3 : 0);
            case Shape h:
                {
                    var r = h.Area;
                    if (h.Oval)
                    {
                        double rx = r.Width / 2, ry = r.Height / 2;
                        if (rx < 1 || ry < 1) return false;
                        double dx = (p.X - (r.X + rx)) / rx, dy = (p.Y - (r.Y + ry)) / ry;
                        return Math.Abs(Math.Sqrt(dx * dx + dy * dy) - 1) * Math.Min(rx, ry) <= tol + h.Width / 2;
                    }
                    double ex = Math.Max(r.X - p.X, p.X - (r.X + r.Width)), ey = Math.Max(r.Y - p.Y, p.Y - (r.Y + r.Height));
                    double d = ex > 0 || ey > 0 ? Math.Sqrt(Math.Max(ex, 0) * Math.Max(ex, 0) + Math.Max(ey, 0) * Math.Max(ey, 0)) : -Math.Max(ex, ey);
                    return Math.Abs(d) <= tol + h.Width / 2;
                }
            case Pixel px: return px.Area.Contains(new Point(p.X, p.Y));
            case Label l: return LabelBox(l).Contains(new Point(p.X, p.Y));
            case Step st: return Vector2.Distance(p, st.At) <= Math.Max(st.Width * 3, 13) + tol;
        }
        return false;
    }

    private int HitAnn(Vector2 p)
    {
        var tol = 6 / _scale;
        for (int i = _anns.Count - 1; i >= 0; i--) if (Hit(_anns[i], p, tol)) return i;
        return -1;
    }

    private static Ann Translate(Ann a, Vector2 d) => a switch
    {
        Stroke s => s with { Points = s.Points.Select(x => x + d).ToList() },
        Seg g => g with { From = g.From + d, To = g.To + d },
        Shape h => h with { Area = new Rect(h.Area.X + d.X, h.Area.Y + d.Y, h.Area.Width, h.Area.Height) },
        Pixel p => p with { Area = new Rect(p.Area.X + d.X, p.Area.Y + d.Y, p.Area.Width, p.Area.Height) },
        Label l => l with { At = l.At + d },
        Step st => st with { At = st.At + d },
        _ => a,
    };

    private static Ann Resized(Ann a, int idx, Vector2 p) => a switch
    {
        Shape h => h with { Area = ResizeRect(h.Area, idx, p) },
        Pixel px => px with { Area = ResizeRect(px.Area, idx, p) },
        Seg g => idx == 0 ? g with { From = p } : g with { To = p },
        _ => a,
    };

    private static string KindOf(Ann a) => a switch
    {
        Stroke s => s.Marker ? "highlighter" : "pen",
        Seg g => g.Head ? "arrow" : "line",
        Shape h => h.Oval ? "ellipse" : "box",
        Pixel => "pixelate",
        Label => "text",
        _ => "step",
    };

    // ------------------------------------------------------------ tools + undo

    private void Tool_Click(object sender, RoutedEventArgs e) => SetTool((string)((FrameworkElement)sender).Tag);

    private void SetTool(string tool)
    {
        CommitText();
        _tool = tool;
        if (tool != "select") _sel = -1;
        _stylePushed = false;
        foreach (var b in Rail.Children.OfType<ToggleButton>()) b.IsChecked = (string)b.Tag == tool;
        _cropDraft = tool == "crop" && _bitmap != null ? _crop ?? Full : null;
        Hint.Text = tool switch
        {
            "select" => "Click to select · drag to move · double-click text to edit · scroll to zoom · Space+drag to pan",
            "crop" => "Drag to choose the area to keep, Enter to apply",
            "pen" => "Draw freely",
            "highlighter" => "Drag over text to highlight it",
            "arrow" => "Drag to draw · Shift snaps to 45°",
            "line" => "Drag to draw · Shift snaps to 45°",
            "box" => "Drag to draw · Shift for a square",
            "ellipse" => "Drag to draw · Shift for a circle",
            "text" => "Click where the text goes",
            "pixelate" => "Drag over anything private",
            "step" => "Click to place the next number",
            _ => "",
        };
        UpdateInspector();
        UpdateCropInfo();
        Redraw();
    }

    private void Push()
    {
        _undo.Push(new State(new List<Ann>(_anns), _crop));
        _redo.Clear();
    }

    private void Undo_Click(object sender, RoutedEventArgs e) => Undo();
    private void Redo_Click(object sender, RoutedEventArgs e) => Redo();

    private void Undo()
    {
        if (_undo.Count == 0) return;
        _redo.Push(new State(new List<Ann>(_anns), _crop));
        var s = _undo.Pop();
        (_anns, _crop) = (s.Anns, s.Crop);
        AfterHistory();
    }

    private void Redo()
    {
        if (_redo.Count == 0) return;
        _undo.Push(new State(new List<Ann>(_anns), _crop));
        var s = _redo.Pop();
        (_anns, _crop) = (s.Anns, s.Crop);
        AfterHistory();
    }

    private void AfterHistory()
    {
        CommitText(cancel: true);
        _sel = -1;
        _stylePushed = false;
        if (_tool == "crop") _cropDraft = _crop ?? Full;
        UpdateInspector();
        UpdateCropInfo();
        Redraw();
    }

    // ------------------------------------------------------------ inspector

    private void HighlightSwatch()
    {
        var cur = (uint)(_color.R << 16 | _color.G << 8 | _color.B);
        var neutral = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        foreach (var b in Swatches.Children.OfType<Button>())
        {
            var on = (uint)b.Tag == cur;
            b.BorderThickness = new Thickness(on ? 3 : 1);
            b.BorderBrush = on ? new SolidColorBrush(_accent) : neutral;
        }
    }

    private void UpdateInspector()
    {
        if (ColorPanel == null) return;
        var sel = _sel >= 0 && _sel < _anns.Count;
        var key = sel ? KindOf(_anns[_sel]) : _tool;
        InspectorTitle.Text = key switch
        {
            "select" => "Select",
            "crop" => "Crop",
            "pen" => "Pen",
            "highlighter" => "Highlighter",
            "arrow" => "Arrow",
            "line" => "Line",
            "box" => "Rectangle",
            "ellipse" => "Ellipse",
            "text" => "Text",
            "step" => "Step number",
            "pixelate" => "Pixelate",
            _ => "",
        };
        ColorPanel.Visibility = key is "select" or "crop" or "pixelate" ? Visibility.Collapsed : Visibility.Visible;
        WidthPanel.Visibility = key is "pen" or "highlighter" or "arrow" or "line" or "box" or "ellipse" or "step" ? Visibility.Visible : Visibility.Collapsed;
        FontPanel.Visibility = key == "text" ? Visibility.Visible : Visibility.Collapsed;
        CropPanel.Visibility = !sel && _tool == "crop" ? Visibility.Visible : Visibility.Collapsed;
        DeleteBtn.Visibility = sel ? Visibility.Visible : Visibility.Collapsed;
        InspectorTip.Text = sel ? "" : _tool switch
        {
            "select" => "Click an annotation to change its colour, size or text. Drag to move it, drag a handle to resize. Delete removes it.",
            "crop" => "Drag the corners or edges, or drag inside to move the frame.",
            _ => "Everything you draw can be selected and changed afterwards with the Select tool (V).",
        };
        if (sel)
        {
            _syncing = true;
            var a = _anns[_sel];
            _color = a.Color;
            if (a is Label l) FontSlider.Value = l.Size;
            else if (a is not Pixel) WidthSlider.Value = a.Width;
            _syncing = false;
            HighlightSwatch();
        }
    }

    private void UpdateCropInfo()
    {
        if (CropInfo == null) return;
        var r = _cropDraft ?? _crop;
        CropInfo.Text = r is Rect c ? $"{Math.Round(c.Width)} × {Math.Round(c.Height)}" : "";
    }

    private void ApplyStyle()
    {
        if (_syncing || _sel < 0 || _sel >= _anns.Count) return;
        if (!_stylePushed) { Push(); _stylePushed = true; }
        var a = _anns[_sel];
        _anns[_sel] = a is Label l ? l with { Color = _color, Size = _font } : a with { Color = _color, Width = _width };
        Surface.Invalidate();
    }

    private void WidthSlider_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        _width = (float)e.NewValue;
        if (WidthLabel != null) WidthLabel.Text = $"Thickness · {(int)e.NewValue} px";
        ApplyStyle();
    }

    private void FontSlider_ValueChanged(object sender, RangeBaseValueChangedEventArgs e)
    {
        _font = (float)e.NewValue;
        if (FontLabel != null) FontLabel.Text = $"Text size · {(int)e.NewValue} px";
        ApplyStyle();
    }

    private void Delete_Click(object sender, RoutedEventArgs e) => DeleteSelected();

    private void DeleteSelected()
    {
        if (_sel < 0 || _sel >= _anns.Count) return;
        Push();
        _anns.RemoveAt(_sel);
        _sel = -1;
        UpdateInspector();
        Surface.Invalidate();
    }

    // ------------------------------------------------------------ pointer

    private static bool Down(VirtualKey k) =>
        InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);

    private void Surface_PointerPressed(object sender, PointerRoutedEventArgs e)
    {
        if (_bitmap == null) return;
        var pt = e.GetCurrentPoint(Surface);
        var props = pt.Properties;
        if (!props.IsLeftButtonPressed && !props.IsMiddleButtonPressed) return;
        CommitText();
        Surface.CapturePointer(e.Pointer);
        var screen = V(pt.Position);
        var raw = ToImage(pt.Position);
        var p = Clamp(raw);
        _start = p;
        _startRaw = raw;
        _moved = false;

        if (props.IsMiddleButtonPressed || Down(VirtualKey.Space)) { BeginPan(screen); return; }

        switch (_tool)
        {
            case "select":
                {
                    if (_sel >= 0 && _sel < _anns.Count)
                    {
                        var h = HitHandle(Handles(_anns[_sel]), screen);
                        if (h >= 0) { _mode = Mode.Resize; _handle = h; _orig = _anns[_sel]; return; }
                    }
                    var idx = HitAnn(raw);
                    if (idx >= 0)
                    {
                        _sel = idx;
                        _stylePushed = false;
                        _mode = Mode.Move;
                        _orig = _anns[idx];
                        UpdateInspector();
                    }
                    else
                    {
                        if (_sel >= 0) { _sel = -1; UpdateInspector(); }
                        BeginPan(screen);
                    }
                    break;
                }
            case "crop":
                {
                    var d = _cropDraft ?? Full;
                    _origRect = d;
                    var isFull = d.Width >= _bitmap.Size.Width - 0.5 && d.Height >= _bitmap.Size.Height - 0.5;
                    var h = isFull ? -1 : HitHandle(HandlePts(d), screen);
                    if (h >= 0) { _mode = Mode.CropResize; _handle = h; }
                    else if (!isFull && d.Contains(new Point(raw.X, raw.Y))) _mode = Mode.CropMove;
                    else { _mode = Mode.CropNew; _cropDraft = new Rect(p.X, p.Y, 0, 0); }
                    break;
                }
            case "pen": _draft = new Stroke(_color, _width, new List<Vector2> { p }, false); _mode = Mode.Draw; break;
            case "highlighter": _draft = new Stroke(_color, _width, new List<Vector2> { p }, true); _mode = Mode.Draw; break;
            case "arrow": _draft = new Seg(_color, _width, p, p, true); _mode = Mode.Draw; break;
            case "line": _draft = new Seg(_color, _width, p, p, false); _mode = Mode.Draw; break;
            case "box": _draft = new Shape(_color, _width, new Rect(p.X, p.Y, 0, 0), false); _mode = Mode.Draw; break;
            case "ellipse": _draft = new Shape(_color, _width, new Rect(p.X, p.Y, 0, 0), true); _mode = Mode.Draw; break;
            case "pixelate": _draft = new Pixel(_color, _width, new Rect(p.X, p.Y, 0, 0)); _mode = Mode.Draw; break;
            case "step":
                Push();
                _anns.Add(new Step(_color, _width, p, _anns.OfType<Step>().Count() + 1));
                _mode = Mode.None;
                break;
            case "text":
                _mode = Mode.None;
                ShowText(p, "", _font);
                break;
        }
        Surface.Invalidate();
    }

    private void BeginPan(Vector2 screen)
    {
        _mode = Mode.Pan;
        _panStart = screen;
        _panOff = _offset;
    }

    private void Surface_PointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (_bitmap == null || _mode == Mode.None) return;
        var pt = e.GetCurrentPoint(Surface);
        var screen = V(pt.Position);
        var raw = ToImage(pt.Position);
        var p = Clamp(raw);
        var shift = Down(VirtualKey.Shift);
        switch (_mode)
        {
            case Mode.Pan:
                _offset = _panOff + (screen - _panStart);
                _fitted = false;
                break;
            case Mode.Draw:
                switch (_draft)
                {
                    case Stroke s:
                        if (s.Points.Count == 0 || Vector2.Distance(s.Points[^1], p) > 1.5f / _scale) s.Points.Add(p);
                        break;
                    case Seg g:
                        if (shift)
                        {
                            var d = p - g.From;
                            var ang = Math.Round(Math.Atan2(d.Y, d.X) / (Math.PI / 4)) * (Math.PI / 4);
                            p = g.From + new Vector2((float)Math.Cos(ang), (float)Math.Sin(ang)) * d.Length();
                        }
                        _draft = g with { To = p };
                        break;
                    case Shape h: _draft = h with { Area = RectFrom(_start, Square(p, shift)) }; break;
                    case Pixel x: _draft = x with { Area = RectFrom(_start, p) }; break;
                }
                break;
            case Mode.Move:
            case Mode.Resize:
                if (_orig == null || _sel < 0 || _sel >= _anns.Count) break;
                if (!_moved)
                {
                    if (Vector2.Distance(ToScreen(raw), ToScreen(_startRaw)) < 3) break;
                    Push();
                    _moved = true;
                }
                _anns[_sel] = _mode == Mode.Move ? Translate(_orig, raw - _startRaw) : Resized(_orig, _handle, raw);
                break;
            case Mode.CropNew:
                _cropDraft = RectFrom(_start, p);
                break;
            case Mode.CropResize:
                _cropDraft = ResizeRect(_origRect, _handle, p);
                break;
            case Mode.CropMove:
                {
                    var d = raw - _startRaw;
                    var x = Math.Clamp(_origRect.X + d.X, 0, _bitmap.Size.Width - _origRect.Width);
                    var y = Math.Clamp(_origRect.Y + d.Y, 0, _bitmap.Size.Height - _origRect.Height);
                    _cropDraft = new Rect(x, y, _origRect.Width, _origRect.Height);
                    break;
                }
        }
        if (_mode is Mode.CropNew or Mode.CropResize or Mode.CropMove) UpdateCropInfo();
        Redraw();
    }

    private Vector2 Square(Vector2 p, bool shift)
    {
        if (!shift) return p;
        var d = p - _start;
        var m = Math.Max(Math.Abs(d.X), Math.Abs(d.Y));
        return _start + new Vector2(Math.Sign(d.X) * m, Math.Sign(d.Y) * m);
    }

    private void Surface_PointerReleased(object sender, PointerRoutedEventArgs e)
    {
        Surface.ReleasePointerCapture(e.Pointer);
        Finish();
    }

    private void Surface_CaptureLost(object sender, PointerRoutedEventArgs e) => Finish();

    private void Finish()
    {
        var mode = _mode;
        _mode = Mode.None;
        if (mode == Mode.Draw && _draft != null)
        {
            var keep = _draft switch
            {
                Seg g => Vector2.Distance(g.From, g.To) > 3,
                Shape h => h.Area.Width > 3 && h.Area.Height > 3,
                Pixel x => x.Area.Width > 3 && x.Area.Height > 3,
                _ => true,
            };
            if (keep) { Push(); _anns.Add(_draft); }
            _draft = null;
        }
        else if (mode is Mode.CropNew && _cropDraft is Rect r && (r.Width < 8 || r.Height < 8))
        {
            _cropDraft = _origRect;
        }
        _orig = null;
        UpdateCropInfo();
        Surface.Invalidate();
    }

    private void Surface_DoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        if (_bitmap == null || _tool != "select") return;
        var idx = HitAnn(ToImage(e.GetPosition(Surface)));
        if (idx >= 0 && _anns[idx] is Label l)
        {
            _sel = idx;
            _editIdx = idx;
            UpdateInspector();
            ShowText(l.At, l.Text, l.Size);
        }
    }

    // ------------------------------------------------------------ crop

    private void ApplyCrop_Click(object sender, RoutedEventArgs e) => ApplyCrop();

    private void ApplyCrop()
    {
        if (_cropDraft is not Rect r || _bitmap == null) return;
        var c = new Rect(Math.Round(r.X), Math.Round(r.Y), Math.Round(r.Width), Math.Round(r.Height));
        var full = c.Width >= _bitmap.Size.Width && c.Height >= _bitmap.Size.Height;
        Push();
        _crop = full ? null : c;
        SetTool("select");
        SavedText.Text = _crop is Rect k ? $"Crop set: {k.Width} × {k.Height} (applied when you save or copy)" : "";
    }

    private void ResetCrop_Click(object sender, RoutedEventArgs e)
    {
        if (_bitmap == null) return;
        if (_crop != null) Push();
        _crop = null;
        _cropDraft = Full;
        UpdateCropInfo();
        Surface.Invalidate();
    }

    // ------------------------------------------------------------ text

    private void ShowText(Vector2 at, string text, float size)
    {
        _textAt = at;
        var s = ToScreen(at);
        Canvas.SetLeft(TextEntry, s.X - 10);
        Canvas.SetTop(TextEntry, s.Y - 8);
        TextEntry.FontSize = Math.Max(11, size * _scale);
        TextEntry.Text = text;
        TextEntry.Visibility = Visibility.Visible;
        TextEntry.Focus(FocusState.Programmatic);
        TextEntry.SelectAll();
        Surface.Invalidate();
    }

    private void CommitText(bool cancel = false)
    {
        if (TextEntry.Visibility != Visibility.Visible) return;
        var text = TextEntry.Text.Trim();
        var at = _textAt;
        var idx = _editIdx;
        _textAt = null;
        _editIdx = -1;
        TextEntry.Visibility = Visibility.Collapsed;
        if (!cancel && at is Vector2 v)
        {
            if (idx >= 0 && idx < _anns.Count && _anns[idx] is Label old)
            {
                if (text != old.Text)
                {
                    Push();
                    if (text.Length == 0) { _anns.RemoveAt(idx); _sel = -1; }
                    else _anns[idx] = old with { Text = text };
                }
            }
            else if (text.Length > 0)
            {
                Push();
                _anns.Add(new Label(_color, _width, v, text, _font));
            }
        }
        UpdateInspector();
        Surface.Invalidate();
    }

    private void TextEntry_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter && !Down(VirtualKey.Shift)) { CommitText(); e.Handled = true; }
        else if (e.Key == VirtualKey.Escape) { CommitText(cancel: true); e.Handled = true; }
    }

    private void TextEntry_LostFocus(object sender, RoutedEventArgs e) => CommitText();

    // ------------------------------------------------------------ output

    private async Task<IRandomAccessStream?> RenderPng()
    {
        if (_bitmap == null) return null;
        CommitText();
        var area = _crop ?? Full;
        var device = CanvasDevice.GetSharedDevice();
        using var target = new CanvasRenderTarget(device, (float)area.Width, (float)area.Height, 96);
        using (var ds = target.CreateDrawingSession())
        {
            ds.Clear(Colors.Transparent);
            ds.Transform = Matrix3x2.CreateTranslation((float)-area.X, (float)-area.Y);
            ds.DrawImage(_bitmap);
            DrawAnns(ds, _bitmap, view: false);
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
        void Key(VirtualKey k, VirtualKeyModifiers m, Func<Task> act, Func<bool>? when = null)
        {
            var ka = new KeyboardAccelerator { Key = k, Modifiers = m };
            ka.Invoked += async (_, e) =>
            {
                if (TextEntry.Visibility == Visibility.Visible || ImageEditor.Visibility != Visibility.Visible) return;
                if (when != null && !when()) return;
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
        foreach (var (k, t) in new[] { (VirtualKey.V, "select"), (VirtualKey.C, "crop"), (VirtualKey.P, "pen"), (VirtualKey.H, "highlighter"),
                     (VirtualKey.A, "arrow"), (VirtualKey.L, "line"), (VirtualKey.B, "box"), (VirtualKey.E, "ellipse"),
                     (VirtualKey.T, "text"), (VirtualKey.N, "step"), (VirtualKey.X, "pixelate") })
            Key(k, VirtualKeyModifiers.None, () => Sync(() => SetTool(t)));
        Key(VirtualKey.Delete, VirtualKeyModifiers.None, () => Sync(DeleteSelected), () => _sel >= 0);
        Key(VirtualKey.Enter, VirtualKeyModifiers.None, () => Sync(ApplyCrop), () => _tool == "crop");
        Key(VirtualKey.Escape, VirtualKeyModifiers.None, () => Sync(() =>
        {
            if (_tool == "crop") SetTool("select");
            else { _sel = -1; UpdateInspector(); Surface.Invalidate(); }
        }), () => _tool == "crop" || _sel >= 0);
        Key(VirtualKey.Number0, VirtualKeyModifiers.None, () => Sync(Fit));
        Key(VirtualKey.Number1, VirtualKeyModifiers.None, () => Sync(() => { if (_bitmap != null) SetZoom(1f / Rs, Center); }));
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
