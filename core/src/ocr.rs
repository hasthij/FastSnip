//! Text recognition for Text mode, with Windows.Media.Ocr (built into every
//! Windows 10/11, no download).
//!
//! Speed: the whole monitor is read on a worker thread the moment the screen
//! freezes, so by the time a box is drawn the words are usually already
//! there. Small selections are then read again at 2x for accuracy on tiny UI
//! text, and the sharper result replaces the first one.
//!
//! Windows AI Text Recognizer (NPU) slots in here later; it needs the app to
//! be installed as a package.

use std::sync::mpsc::{channel, Sender};

use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::capture::Rect;

pub const WM_OCR: u32 = WM_APP + 4;

#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    /// Screen coordinates (physical pixels).
    pub rect: Rect,
    /// Line number, unique across one result, in reading order.
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Pre-read of a whole monitor at freeze time.
    Monitor,
    /// Sharper re-read of the selected area.
    Region,
}

pub struct Job {
    pub seq: u64,
    pub kind: Kind,
    pub origin: (i32, i32),
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
    pub upscale: bool,
}

pub struct Done {
    pub seq: u64,
    pub kind: Kind,
    pub area: Rect,
    pub words: Vec<Word>,
    pub ms: f64,
}

pub struct Reader {
    tx: Sender<Job>,
}

impl Reader {
    pub fn start(notify: HWND) -> Self {
        let (tx, rx) = channel::<Job>();
        let target = notify.0 as isize;
        std::thread::Builder::new()
            .name("fastsnip-ocr".into())
            .spawn(move || {
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                }
                let engine = OcrEngine::TryCreateFromUserProfileLanguages().ok();
                while let Ok(job) = rx.recv() {
                    let t = std::time::Instant::now();
                    let area = Rect { x: job.origin.0, y: job.origin.1, w: job.w as i32, h: job.h as i32 };
                    let words = engine.as_ref().and_then(|e| read(e, &job).ok()).unwrap_or_default();
                    let done = Box::new(Done { seq: job.seq, kind: job.kind, area, words, ms: t.elapsed().as_secs_f64() * 1000.0 });
                    unsafe {
                        let _ = PostMessageW(Some(HWND(target as *mut _)), WM_OCR, WPARAM(0), LPARAM(Box::into_raw(done) as isize));
                    }
                }
            })
            .expect("ocr thread");
        Self { tx }
    }

    pub fn read(&self, job: Job) {
        let _ = self.tx.send(job);
    }
}

/// Read one job on the calling thread (benchmarks and tests).
pub fn read_now(job: &Job) -> Vec<Word> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
        .ok()
        .and_then(|e| read(&e, job).ok())
        .unwrap_or_default()
}

/// 2x bilinear upscale of BGRA.
fn upscale2(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let (w2, h2) = (w * 2, h * 2);
    let mut out = vec![0u8; w2 * h2 * 4];
    for y in 0..h2 {
        let fy = (y as f32 + 0.5) / 2.0 - 0.5;
        let y0 = fy.floor().max(0.0) as usize;
        let y1 = (y0 + 1).min(h - 1);
        let ty = (fy - y0 as f32).clamp(0.0, 1.0);
        for x in 0..w2 {
            let fx = (x as f32 + 0.5) / 2.0 - 0.5;
            let x0 = fx.floor().max(0.0) as usize;
            let x1 = (x0 + 1).min(w - 1);
            let tx = (fx - x0 as f32).clamp(0.0, 1.0);
            for c in 0..4 {
                let p = |xx: usize, yy: usize| src[(yy * w + xx) * 4 + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bot = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                out[(y * w2 + x) * 4 + c] = (top * (1.0 - ty) + bot * ty).round() as u8;
            }
        }
    }
    out
}

fn read(engine: &OcrEngine, job: &Job) -> windows::core::Result<Vec<Word>> {
    let max = OcrEngine::MaxImageDimension().unwrap_or(4096);
    let up = job.upscale && job.w * 2 <= max && job.h * 2 <= max;
    let (pixels, w, h, k) = if up {
        (upscale2(&job.bgra, job.w, job.h), job.w * 2, job.h * 2, 0.5f32)
    } else {
        (job.bgra.clone(), job.w, job.h, 1.0f32)
    };
    if w > max || h > max {
        return Ok(Vec::new());
    }
    let writer = DataWriter::new()?;
    writer.WriteBytes(&pixels)?;
    let buf = writer.DetachBuffer()?;
    let bmp = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buf, BitmapPixelFormat::Bgra8, w as i32, h as i32, BitmapAlphaMode::Ignore)?;
    let result = engine.RecognizeAsync(&bmp)?.join()?;
    let mut words = Vec::new();
    for (li, line) in result.Lines()?.into_iter().enumerate() {
        for word in line.Words()? {
            let r = word.BoundingRect()?;
            words.push(Word {
                text: word.Text()?.to_string(),
                rect: Rect {
                    x: job.origin.0 + (r.X * k).round() as i32,
                    y: job.origin.1 + (r.Y * k).round() as i32,
                    w: (r.Width * k).round().max(1.0) as i32,
                    h: (r.Height * k).round().max(1.0) as i32,
                },
                line: li,
            });
        }
    }
    Ok(words)
}

/// Words whose center is inside `area`, in reading order.
pub fn words_in(words: &[Word], area: &Rect) -> Vec<Word> {
    let mut v: Vec<Word> = words
        .iter()
        .filter(|w| area.contains(w.rect.x + w.rect.w / 2, w.rect.y + w.rect.h / 2))
        .cloned()
        .collect();
    sort_reading(&mut v);
    v
}

/// Sort by line, then left to right, and renumber lines from 0.
pub fn sort_reading(v: &mut [Word]) {
    v.sort_by(|a, b| a.line.cmp(&b.line).then(a.rect.x.cmp(&b.rect.x)));
    let mut last = usize::MAX;
    let mut n = 0usize;
    for w in v.iter_mut() {
        if w.line != last {
            if last != usize::MAX {
                n += 1;
            }
            last = w.line;
        }
        w.line = n;
    }
}

/// Join selected words: spaces within a line, a space or newline between lines.
pub fn join(words: &[Word], keep_lines: bool) -> String {
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for w in words {
        if let Some(p) = prev {
            out.push(if keep_lines && p != w.line { '\n' } else { ' ' });
        }
        out.push_str(&w.text);
        prev = Some(w.line);
    }
    out
}

/// A link or email address in the text, as something the shell can open.
pub fn find_link(text: &str) -> Option<(String, String)> {
    for tok in text.split_whitespace() {
        let t = tok.trim_matches(|c: char| ",.;:()[]<>\"'".contains(c));
        if t.contains('@') && t.contains('.') && !t.starts_with('@') {
            return Some((format!("Email {t}"), format!("mailto:{t}")));
        }
        if t.starts_with("http://") || t.starts_with("https://") {
            return Some((format!("Open {t}"), t.to_string()));
        }
        if let Some((host, _)) = t.split_once('/').or(Some((t, ""))) {
            let parts: Vec<&str> = host.split('.').collect();
            let tld_ok = parts.last().map(|l| l.len() >= 2 && l.chars().all(|c| c.is_ascii_alphabetic())).unwrap_or(false);
            if parts.len() >= 2 && tld_ok && parts.iter().all(|p| !p.is_empty()) && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
                return Some((format!("Open {t}"), format!("https://{t}")));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, x: i32, line: usize) -> Word {
        Word { text: text.into(), rect: Rect { x, y: line as i32 * 20, w: 10, h: 10 }, line }
    }

    #[test]
    fn joins_with_and_without_line_breaks() {
        let v = vec![w("a", 0, 0), w("b", 20, 0), w("c", 0, 1)];
        assert_eq!(join(&v, false), "a b c");
        assert_eq!(join(&v, true), "a b\nc");
    }

    #[test]
    fn finds_links() {
        assert_eq!(find_link("write to support@example.com today").unwrap().1, "mailto:support@example.com");
        assert_eq!(find_link("visit example.com/help").unwrap().1, "https://example.com/help");
        assert!(find_link("version 1.4 is out").is_none());
    }

    #[test]
    fn upscale_size() {
        let v = upscale2(&[10, 20, 30, 255].repeat(4), 2, 2);
        assert_eq!(v.len(), 4 * 4 * 4);
        assert_eq!(&v[0..4], &[10, 20, 30, 255]);
    }
}
