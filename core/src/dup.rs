//! Fast screen grab with DXGI Desktop Duplication.
//!
//! The duplication objects stay open while the core is idle. On the hotkey we
//! ask each output for its newest frame without waiting: if nothing changed
//! since last time, the copy we already hold is still exact. The result stays
//! on the GPU and is shown by the overlay directly (no CPU copy). A CPU copy,
//! needed for saving and the color picker, is read back after the overlay is
//! already visible.
//!
//! Anything duplication can't handle (another GPU, secure desktop, lost
//! access) falls back to the GDI grab in capture.rs.

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::capture::{Frame, Rect};

struct Output {
    output: IDXGIOutput1,
    dup: Option<IDXGIOutputDuplication>,
    rect: Rect,
    copy: Option<ID3D11Texture2D>,
    staging: Option<ID3D11Texture2D>,
}

pub struct Dup {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    outputs: Vec<Output>,
}

/// One monitor's frozen image on the GPU.
pub struct GpuShot {
    pub rect: Rect,
    pub tex: ID3D11Texture2D,
}

impl Dup {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let ctx = unsafe { device.GetImmediateContext()? };
        let mut d = Self {
            device: device.clone(),
            ctx,
            outputs: Vec::new(),
        };
        d.enumerate()?;
        // Prime every output so the first hotkey never has to wait for a frame.
        let _ = d.grab_timeout(250);
        Ok(d)
    }

    fn enumerate(&mut self) -> Result<()> {
        self.outputs.clear();
        let adapter = unsafe { self.device.cast::<IDXGIDevice>()?.GetAdapter()? };
        let mut i = 0;
        while let Ok(out) = unsafe { adapter.EnumOutputs(i) } {
            i += 1;
            let Ok(out1) = out.cast::<IDXGIOutput1>() else {
                continue;
            };
            let desc = unsafe { out.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let rect = Rect::from_win(desc.DesktopCoordinates);
            let dup = unsafe { out1.DuplicateOutput(&self.device).ok() };
            self.outputs.push(Output {
                output: out1,
                dup,
                rect,
                copy: None,
                staging: None,
            });
        }
        Ok(())
    }

    /// Re-create everything after a display change.
    pub fn reset(&mut self) {
        let _ = self.enumerate();
        let _ = self.grab_timeout(250);
    }

    pub fn grab(&mut self) -> Vec<GpuShot> {
        self.grab_timeout(0)
    }

    fn grab_timeout(&mut self, timeout_ms: u32) -> Vec<GpuShot> {
        let mut shots = Vec::new();
        for o in &mut self.outputs {
            if o.dup.is_none() {
                o.dup = unsafe { o.output.DuplicateOutput(&self.device).ok() };
            }
            let Some(dup) = o.dup.clone() else { continue };
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            let wait = if o.copy.is_none() {
                timeout_ms.max(100)
            } else {
                timeout_ms
            };
            match unsafe { dup.AcquireNextFrame(wait, &mut info, &mut res) } {
                Ok(()) => {
                    if let Some(src) = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok()) {
                        if o.copy.is_none() {
                            o.copy = make_copy(&self.device, &src).ok();
                        }
                        if let Some(dst) = &o.copy {
                            unsafe { self.ctx.CopyResource(dst, &src) };
                        }
                    }
                    unsafe {
                        let _ = dup.ReleaseFrame();
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    // Nothing changed: the copy we hold is current.
                }
                Err(_) => {
                    // Access lost (mode change, secure desktop). Try again next time.
                    o.dup = None;
                    o.copy = None;
                    o.staging = None;
                    continue;
                }
            }
            if let Some(t) = &o.copy {
                shots.push(GpuShot {
                    rect: o.rect,
                    tex: t.clone(),
                });
            }
        }
        shots
    }

    /// True if every sampled pixel of every shot is pure black. After a lock
    /// screen, UAC prompt or session switch, duplication can hand back an
    /// empty frame; a real desktop is never black at all 64 sample points.
    pub fn looks_black(&mut self, shots: &[GpuShot]) -> bool {
        const N: u32 = 8;
        let all_black = shots.iter().all(|s| unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: N * N,
                Height: 1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut probe = None;
            if self.device.CreateTexture2D(&desc, None, Some(&mut probe)).is_err() {
                return false;
            }
            let probe = probe.unwrap();
            let (w, h) = (s.rect.w as u32, s.rect.h as u32);
            for gy in 0..N {
                for gx in 0..N {
                    let x = (w * (2 * gx + 1)) / (2 * N);
                    let y = (h * (2 * gy + 1)) / (2 * N);
                    let b = D3D11_BOX { left: x, top: y, front: 0, right: x + 1, bottom: y + 1, back: 1 };
                    self.ctx.CopySubresourceRegion(&probe, 0, gy * N + gx, 0, 0, &s.tex, 0, Some(&b));
                }
            }
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if self.ctx.Map(&probe, 0, D3D11_MAP_READ, 0, Some(&mut m)).is_err() {
                return false;
            }
            let px = std::slice::from_raw_parts(m.pData as *const u8, (N * N * 4) as usize);
            let black = px.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
            self.ctx.Unmap(&probe, 0);
            black
        });
        !shots.is_empty() && all_black
    }

    /// CPU copy of the given shots, assembled into one frame covering `bounds`.
    pub fn readback(&mut self, shots: &[GpuShot], bounds: Rect) -> Option<Frame> {
        let stride = bounds.w as usize * 4;
        let mut pixels = vec![0u8; stride * bounds.h as usize];
        for s in shots {
            let o = self.outputs.iter_mut().find(|o| o.rect == s.rect)?;
            if o.staging.is_none() {
                o.staging = make_staging(&self.device, &s.tex).ok();
            }
            let staging = o.staging.as_ref()?;
            unsafe {
                self.ctx.CopyResource(staging, &s.tex);
                let mut m = D3D11_MAPPED_SUBRESOURCE::default();
                self.ctx
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut m))
                    .ok()?;
                let r = s.rect;
                let row = r.w as usize * 4;
                for y in 0..r.h as usize {
                    let src = (m.pData as *const u8).add(y * m.RowPitch as usize);
                    let dy = (r.y - bounds.y) as usize + y;
                    let dx = (r.x - bounds.x) as usize * 4;
                    let dst = pixels.as_mut_ptr().add(dy * stride + dx);
                    std::ptr::copy_nonoverlapping(src, dst, row);
                }
                self.ctx.Unmap(staging, 0);
            }
        }
        Some(Frame { bounds, pixels })
    }
}

fn make_copy(device: &ID3D11Device, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { src.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
    desc.CPUAccessFlags = 0;
    desc.MiscFlags = 0;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
    Ok(t.unwrap())
}

fn make_staging(device: &ID3D11Device, src: &ID3D11Texture2D) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { src.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut t))? };
    Ok(t.unwrap())
}
