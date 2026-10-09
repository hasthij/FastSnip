//! Text mode selection, Live Text style: words are shown in place and the
//! user picks what to copy. Nothing reaches the clipboard until they copy.
//!
//! Selection follows reading order like normal text: dragging from a word on
//! one line to a word on another selects everything in between.

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;

use crate::capture::Rect;
use crate::ocr::{self, Word};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarBtn {
    Copy,
    All,
    Link(String),
    Search,
}

pub struct LiveText {
    pub region: Rect,
    pub words: Vec<Word>,
    pub sel: Option<(usize, usize)>,
    pub selecting: bool,
    /// The sharper re-read of this region has arrived.
    pub refined: bool,
    /// Still waiting for text.
    pub reading: bool,
    anchor: usize,
    last_click: Option<(Instant, (i32, i32), u8)>,
}

impl LiveText {
    pub fn new(region: Rect, words: Vec<Word>, reading: bool) -> Self {
        Self { region, words, sel: None, selecting: false, refined: false, reading, anchor: 0, last_click: None }
    }

    pub fn set_words(&mut self, words: Vec<Word>) {
        self.words = words;
        self.sel = None;
        self.selecting = false;
    }

    fn word_at(&self, p: (i32, i32)) -> Option<usize> {
        let inflate = |r: &Rect, d: i32| Rect { x: r.x - d, y: r.y - d, w: r.w + 2 * d, h: r.h + 2 * d };
        if let Some(i) = self.words.iter().position(|w| inflate(&w.rect, 3).contains(p.0, p.1)) {
            return Some(i);
        }
        self.nearest(p, 28)
    }

    fn nearest(&self, p: (i32, i32), max: i32) -> Option<usize> {
        let mut best = None;
        let mut bd = i64::MAX;
        for (i, w) in self.words.iter().enumerate() {
            let dx = (w.rect.x - p.0).max(0).max(p.0 - w.rect.right()) as i64;
            let dy = (w.rect.y - p.1).max(0).max(p.1 - w.rect.bottom()) as i64;
            let d = dx * dx + dy * dy * 4;
            if d < bd {
                bd = d;
                best = Some(i);
            }
        }
        (bd <= (max as i64 * max as i64)).then_some(best).flatten()
    }

    /// Mouse down inside the region. Returns false if the point is outside it.
    pub fn press(&mut self, p: (i32, i32)) -> bool {
        if !self.region.contains(p.0, p.1) {
            return false;
        }
        let now = Instant::now();
        let dbl = Duration::from_millis(unsafe { GetDoubleClickTime() } as u64);
        let count = match self.last_click {
            Some((t, q, n)) if now - t <= dbl && (q.0 - p.0).abs() <= 4 && (q.1 - p.1).abs() <= 4 => (n % 3) + 1,
            _ => 1,
        };
        self.last_click = Some((now, p, count));
        let Some(i) = self.word_at(p) else {
            self.sel = None;
            return true;
        };
        match count {
            1 => {
                self.anchor = i;
                self.sel = Some((i, i));
                self.selecting = true;
            }
            2 => self.sel = Some((i, i)),
            _ => {
                let line = self.words[i].line;
                let a = self.words.iter().position(|w| w.line == line).unwrap_or(i);
                let b = self.words.iter().rposition(|w| w.line == line).unwrap_or(i);
                self.sel = Some((a, b));
            }
        }
        true
    }

    pub fn drag(&mut self, p: (i32, i32)) {
        if !self.selecting {
            return;
        }
        if let Some(j) = self.word_at(p).or_else(|| self.nearest(p, 400)) {
            self.sel = Some((self.anchor.min(j), self.anchor.max(j)));
        }
    }

    pub fn release(&mut self) {
        self.selecting = false;
    }

    pub fn select_all(&mut self) {
        if !self.words.is_empty() {
            self.sel = Some((0, self.words.len() - 1));
        }
    }

    pub fn selected(&self) -> &[Word] {
        match self.sel {
            Some((a, b)) if b < self.words.len() => &self.words[a..=b],
            _ => &[],
        }
    }

    pub fn text(&self, keep_lines: bool) -> String {
        ocr::join(self.selected(), keep_lines)
    }

    pub fn bar_buttons(&self, keep_lines: bool) -> Vec<(BarBtn, String, &'static str)> {
        let n = self.selected().len();
        let mut v = vec![
            (BarBtn::Copy, format!("Copy {n} {}", if n == 1 { "word" } else { "words" }), "copy"),
            (BarBtn::All, "Select all".to_string(), "text-select"),
        ];
        if let Some((label, url)) = ocr::find_link(&self.text(keep_lines)) {
            let short: String = label.chars().take(34).collect();
            v.push((BarBtn::Link(url), short, "link"));
        }
        v.push((BarBtn::Search, String::new(), "search"));
        v
    }
}

/// "https://www.bing.com/search?q=..." with the text percent-encoded.
pub fn search_url(text: &str) -> String {
    let mut q = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => q.push(b as char),
            b' ' | b'\n' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    format!("https://www.bing.com/search?q={q}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words() -> Vec<Word> {
        let mk = |t: &str, x: i32, line: usize| Word { text: t.into(), rect: Rect { x, y: 10 + line as i32 * 30, w: 40, h: 16 }, line };
        vec![mk("one", 10, 0), mk("two", 60, 0), mk("three", 10, 1), mk("four", 60, 1)]
    }

    #[test]
    fn drag_selects_across_lines_in_reading_order() {
        let mut lt = LiveText::new(Rect { x: 0, y: 0, w: 200, h: 100 }, words(), false);
        assert!(lt.press((70, 15)));
        lt.drag((20, 45));
        lt.release();
        assert_eq!(lt.text(false), "two three");
        assert_eq!(lt.text(true), "two\nthree");
    }

    #[test]
    fn select_all_and_outside_press() {
        let mut lt = LiveText::new(Rect { x: 0, y: 0, w: 200, h: 100 }, words(), false);
        assert!(!lt.press((500, 500)));
        lt.select_all();
        assert_eq!(lt.text(false), "one two three four");
    }

    #[test]
    fn search_url_encodes() {
        assert_eq!(search_url("a b&c"), "https://www.bing.com/search?q=a+b%26c");
    }
}
