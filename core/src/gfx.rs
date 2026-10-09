//! GPU drawing: one D3D11 device and one Direct2D device context shared by all
//! overlay windows. Each window has its own flip-model swap chain.
//!
//! Icons are the Lucide SVGs from assets/icons, drawn with Direct2D's built-in
//! SVG renderer and cached per color.

use std::collections::HashMap;

use windows::core::{Interface, Result, HSTRING};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows_numerics::{Matrix3x2, Vector2};

use crate::theme;

pub const ICONS: &[(&str, &str)] = &[
    ("camera", include_str!("../assets/icons/camera.svg")),
    ("video", include_str!("../assets/icons/video.svg")),
    ("scan-text", include_str!("../assets/icons/scan-text.svg")),
    (
        "square-dashed",
        include_str!("../assets/icons/square-dashed.svg"),
    ),
    ("app-window", include_str!("../assets/icons/app-window.svg")),
    ("scan", include_str!("../assets/icons/scan.svg")),
    ("lasso", include_str!("../assets/icons/lasso.svg")),
    ("monitor", include_str!("../assets/icons/monitor.svg")),
    ("mic", include_str!("../assets/icons/mic.svg")),
    ("mic-off", include_str!("../assets/icons/mic-off.svg")),
    ("volume-2", include_str!("../assets/icons/volume-2.svg")),
    (
        "layout-grid",
        include_str!("../assets/icons/layout-grid.svg"),
    ),
    ("x", include_str!("../assets/icons/x.svg")),
    ("copy", include_str!("../assets/icons/copy.svg")),
    ("search", include_str!("../assets/icons/search.svg")),
    ("link", include_str!("../assets/icons/link.svg")),
    (
        "text-select",
        include_str!("../assets/icons/text-select.svg"),
    ),
    ("circle-dot", include_str!("../assets/icons/circle-dot.svg")),
    ("pause", include_str!("../assets/icons/pause.svg")),
    ("square", include_str!("../assets/icons/square.svg")),
    ("pen-line", include_str!("../assets/icons/pen-line.svg")),
    ("folder-open", include_str!("../assets/icons/folder-open.svg")),
    ("trash-2", include_str!("../assets/icons/trash-2.svg")),
    ("scissors", include_str!("../assets/icons/scissors.svg")),
    ("circle-check", include_str!("../assets/icons/circle-check.svg")),
];

pub fn rf(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

pub struct Fonts {
    pub label: IDWriteTextFormat,
    pub label_bold: IDWriteTextFormat,
    pub key: IDWriteTextFormat,
    pub badge: IDWriteTextFormat,
    pub scale: f32,
}

pub struct Gfx {
    pub d3d: ID3D11Device,
    pub factory: ID2D1Factory1,
    pub dc: ID2D1DeviceContext,
    dc5: Option<ID2D1DeviceContext5>,
    pub dwrite: IDWriteFactory,
    dxgi_factory: IDXGIFactory2,
    pub brush: ID2D1SolidColorBrush,
    svg: HashMap<(String, String), ID2D1SvgDocument>,
    fonts: HashMap<u32, Fonts>,
}

pub struct Surface {
    pub swap: IDXGISwapChain1,
    pub target: Option<ID2D1Bitmap1>,
    pub w: u32,
    pub h: u32,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            let mut d3d: Option<ID3D11Device> = None;
            let levels = [
                D3D_FEATURE_LEVEL_11_1,
                D3D_FEATURE_LEVEL_11_0,
                D3D_FEATURE_LEVEL_10_1,
                D3D_FEATURE_LEVEL_10_0,
            ];
            let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
            let hw = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            );
            if hw.is_err() {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_WARP,
                    HMODULE::default(),
                    flags,
                    Some(&levels),
                    D3D11_SDK_VERSION,
                    Some(&mut d3d),
                    None,
                    None,
                )?;
            }
            let d3d = d3d.unwrap();
            let dxgi_device: IDXGIDevice = d3d.cast()?;
            let factory: ID2D1Factory1 =
                D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let device = factory.CreateDevice(&dxgi_device)?;
            let dc = device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            let dc5 = dc.cast::<ID2D1DeviceContext5>().ok();
            let dxgi_factory: IDXGIFactory2 = dxgi_device.GetAdapter()?.GetParent()?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let brush = dc.CreateSolidColorBrush(&theme::rgb(0), None)?;
            Ok(Self {
                d3d,
                factory,
                dc,
                dc5,
                dwrite,
                dxgi_factory,
                brush,
                svg: HashMap::new(),
                fonts: HashMap::new(),
            })
        }
    }

    pub fn surface(&self, hwnd: HWND, w: u32, h: u32) -> Result<Surface> {
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_NONE,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_IGNORE,
            Flags: 0,
            ..Default::default()
        };
        let swap = unsafe {
            self.dxgi_factory
                .CreateSwapChainForHwnd(&self.d3d, hwnd, &desc, None, None)?
        };
        Ok(Surface {
            swap,
            target: None,
            w,
            h,
        })
    }

    fn target_for(&self, s: &mut Surface) -> Result<ID2D1Bitmap1> {
        if let Some(t) = &s.target {
            return Ok(t.clone());
        }
        unsafe {
            let buf: IDXGISurface = s.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let t = self.dc.CreateBitmapFromDxgiSurface(&buf, Some(&props))?;
            s.target = Some(t.clone());
            Ok(t)
        }
    }

    pub fn resize(&self, s: &mut Surface, w: u32, h: u32) -> Result<()> {
        if s.w == w && s.h == h {
            return Ok(());
        }
        s.target = None;
        unsafe {
            self.dc.SetTarget(None);
            s.swap
                .ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
        }
        s.w = w;
        s.h = h;
        Ok(())
    }

    pub fn begin(&self, s: &mut Surface) -> Result<()> {
        let t = self.target_for(s)?;
        unsafe {
            self.dc.SetTarget(&t);
            self.dc.SetDpi(96.0, 96.0);
            self.dc.BeginDraw();
            self.dc.SetTransform(&Matrix3x2::identity());
        }
        Ok(())
    }

    pub fn end(&self, s: &Surface) -> Result<()> {
        unsafe {
            self.dc.EndDraw(None, None)?;
            self.dc.SetTarget(None);
            s.swap.Present(0, DXGI_PRESENT(0)).ok()
        }
    }

    /// Upload a region of BGRA pixels (with `stride` bytes per row) as a drawable bitmap.
    pub fn bitmap(&self, ptr: *const u8, w: u32, h: u32, stride: u32) -> Result<ID2D1Bitmap1> {
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        unsafe {
            self.dc.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                Some(ptr as *const _),
                stride,
                &props,
            )
        }
    }

    /// Wrap a GPU texture (from desktop duplication) as a drawable bitmap. No copy.
    pub fn bitmap_from_texture(&self, tex: &ID3D11Texture2D) -> Result<ID2D1Bitmap1> {
        let surface: IDXGISurface = tex.cast()?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_IGNORE,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        unsafe { self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props)) }
    }

    pub fn fonts(&mut self, scale: f32) -> &Fonts {
        let key = (scale * 100.0) as u32;
        if !self.fonts.contains_key(&key) {
            let mk = |family: &str, weight: DWRITE_FONT_WEIGHT, size: f32| unsafe {
                let f = self
                    .dwrite
                    .CreateTextFormat(
                        &HSTRING::from(family),
                        None,
                        weight,
                        DWRITE_FONT_STYLE_NORMAL,
                        DWRITE_FONT_STRETCH_NORMAL,
                        size * scale,
                        &HSTRING::from("en-us"),
                    )
                    .unwrap();
                let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                f
            };
            let fonts = Fonts {
                label: mk("Segoe UI Variable Text", DWRITE_FONT_WEIGHT_MEDIUM, 13.0),
                label_bold: mk("Segoe UI Variable Text", DWRITE_FONT_WEIGHT_SEMI_BOLD, 13.0),
                key: mk("Cascadia Mono", DWRITE_FONT_WEIGHT_MEDIUM, 10.0),
                badge: mk("Cascadia Mono", DWRITE_FONT_WEIGHT_MEDIUM, 11.5),
                scale,
            };
            self.fonts.insert(key, fonts);
        }
        &self.fonts[&key]
    }

    pub fn color(&self, c: D2D1_COLOR_F) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&c) };
        &self.brush
    }

    pub fn fill(&self, r: D2D_RECT_F, c: D2D1_COLOR_F) {
        unsafe { self.dc.FillRectangle(&r, self.color(c)) }
    }

    pub fn fill_round(&self, r: D2D_RECT_F, radius: f32, c: D2D1_COLOR_F) {
        let rr = D2D1_ROUNDED_RECT {
            rect: r,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe { self.dc.FillRoundedRectangle(&rr, self.color(c)) }
    }

    pub fn stroke_round(&self, r: D2D_RECT_F, radius: f32, c: D2D1_COLOR_F, width: f32) {
        let rr = D2D1_ROUNDED_RECT {
            rect: r,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            self.dc
                .DrawRoundedRectangle(&rr, self.color(c), width, None)
        }
    }

    pub fn stroke(
        &self,
        r: D2D_RECT_F,
        c: D2D1_COLOR_F,
        width: f32,
        dashed: Option<&ID2D1StrokeStyle>,
    ) {
        unsafe { self.dc.DrawRectangle(&r, self.color(c), width, dashed) }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: D2D1_COLOR_F, width: f32) {
        unsafe {
            self.dc.DrawLine(
                Vector2 { X: x0, Y: y0 },
                Vector2 { X: x1, Y: y1 },
                self.color(c),
                width,
                None,
            )
        }
    }

    pub fn dash_style(&self) -> Option<ID2D1StrokeStyle> {
        let props = D2D1_STROKE_STYLE_PROPERTIES1 {
            transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
            startCap: D2D1_CAP_STYLE_FLAT,
            endCap: D2D1_CAP_STYLE_FLAT,
            dashCap: D2D1_CAP_STYLE_FLAT,
            lineJoin: D2D1_LINE_JOIN_MITER,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_CUSTOM,
            dashOffset: 0.0,
        };
        let dashes = [4.0f32, 3.0];
        unsafe {
            self.factory
                .CreateStrokeStyle(&props, Some(&dashes))
                .ok()
                .map(|s| s.cast().unwrap())
        }
    }

    pub fn text_size(&self, text: &str, fmt: &IDWriteTextFormat) -> (f32, f32) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            if let Ok(layout) = self.dwrite.CreateTextLayout(&wide, fmt, 10000.0, 10000.0) {
                let mut m = DWRITE_TEXT_METRICS::default();
                if layout.GetMetrics(&mut m).is_ok() {
                    return (m.widthIncludingTrailingWhitespace, m.height);
                }
            }
        }
        (0.0, 0.0)
    }

    /// Draw text with its top-left at (x, y).
    pub fn text(&self, text: &str, fmt: &IDWriteTextFormat, x: f32, y: f32, c: D2D1_COLOR_F) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dc.DrawText(
                &wide,
                fmt,
                &rf(x, y, 10000.0, 10000.0),
                self.color(c),
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// Text centered in a box.
    pub fn text_center(&self, text: &str, fmt: &IDWriteTextFormat, r: D2D_RECT_F, c: D2D1_COLOR_F) {
        let (w, h) = self.text_size(text, fmt);
        let x = r.left + ((r.right - r.left) - w) / 2.0;
        let y = r.top + ((r.bottom - r.top) - h) / 2.0;
        self.text(text, fmt, x, y, c);
    }

    /// Draw a Lucide icon in a `size` x `size` box at (x, y).
    pub fn icon(&mut self, name: &str, x: f32, y: f32, size: f32, c: D2D1_COLOR_F) {
        let Some(dc5) = self.dc5.clone() else { return };
        let color = theme::css(c);
        let key = (name.to_string(), color.clone());
        if !self.svg.contains_key(&key) {
            let Some(src) = ICONS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s) else {
                return;
            };
            let svg = src.replace("currentColor", &color);
            let doc = unsafe {
                let Some(stream) = SHCreateMemStream(Some(svg.as_bytes())) else {
                    return;
                };
                match dc5.CreateSvgDocument(
                    &stream,
                    D2D_SIZE_F {
                        width: 24.0,
                        height: 24.0,
                    },
                ) {
                    Ok(d) => d,
                    Err(_) => return,
                }
            };
            self.svg.insert(key.clone(), doc);
        }
        let doc = &self.svg[&key];
        let s = size / 24.0;
        unsafe {
            let mut old = Matrix3x2::identity();
            self.dc.GetTransform(&mut old);
            self.dc
                .SetTransform(&(Matrix3x2::scale(s, s) * Matrix3x2::translation(x, y) * old));
            dc5.DrawSvgDocument(doc);
            self.dc.SetTransform(&old);
        }
    }

    /// Fill a polygon (used for the freeform shape outline and its undimmed area).
    pub fn polygon(&self, pts: &[(f32, f32)]) -> Option<ID2D1PathGeometry> {
        if pts.len() < 3 {
            return None;
        }
        unsafe {
            let geo = self.factory.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            sink.BeginFigure(
                Vector2 {
                    X: pts[0].0,
                    Y: pts[0].1,
                },
                D2D1_FIGURE_BEGIN_FILLED,
            );
            let rest: Vec<Vector2> = pts[1..]
                .iter()
                .map(|p| Vector2 { X: p.0, Y: p.1 })
                .collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close().ok()?;
            geo.cast().ok()
        }
    }
}
