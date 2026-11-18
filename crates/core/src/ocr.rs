//! OCR of rendered pages with Tesseract (via its stable C API).
//!
//! Tesseract returns words with pixel bounding boxes; these are converted into
//! pseudo-glyphs so the regular layout analyser can reconstruct lines,
//! paragraphs, columns and tables exactly as for born-digital PDFs.

use image::{DynamicImage, GenericImageView};

use crate::model::*;

/// A recognised word in page coordinates (points).
#[derive(Clone, Debug)]
pub struct OcrWord {
    pub text: String,
    pub bbox: Rect,
    pub confidence: f32,
    /// (block, paragraph, line) identifiers from Tesseract.
    pub line_key: (u32, u32, u32),
}

#[derive(Clone, Debug)]
pub struct OcrSettings {
    /// Tesseract language string, e.g. `"eng"` or `"eng+deu"`.
    pub language: String,
    /// Directory containing `*.traineddata` (None = Tesseract default / TESSDATA_PREFIX).
    pub tessdata: Option<std::path::PathBuf>,
    /// Font family used for recognised text.
    pub font: String,
    /// Words below this confidence that contain no alphanumerics are dropped.
    pub min_confidence: f32,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            language: "eng".into(),
            tessdata: None,
            font: "Times New Roman".into(),
            min_confidence: 30.0,
        }
    }
}

#[cfg(feature = "ocr")]
mod ffi {
    use std::os::raw::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct TessBaseAPI {
        _private: [u8; 0],
    }

    extern "C" {
        pub fn TessVersion() -> *const c_char;
        pub fn TessBaseAPICreate() -> *mut TessBaseAPI;
        pub fn TessBaseAPIDelete(handle: *mut TessBaseAPI);
        pub fn TessBaseAPIInit3(handle: *mut TessBaseAPI, datapath: *const c_char, language: *const c_char) -> c_int;
        pub fn TessBaseAPISetPageSegMode(handle: *mut TessBaseAPI, mode: c_int);
        pub fn TessBaseAPISetVariable(handle: *mut TessBaseAPI, name: *const c_char, value: *const c_char) -> c_int;
        pub fn TessBaseAPISetImage(
            handle: *mut TessBaseAPI,
            imagedata: *const u8,
            width: c_int,
            height: c_int,
            bytes_per_pixel: c_int,
            bytes_per_line: c_int,
        );
        pub fn TessBaseAPISetSourceResolution(handle: *mut TessBaseAPI, ppi: c_int);
        pub fn TessBaseAPIRecognize(handle: *mut TessBaseAPI, monitor: *mut c_void) -> c_int;
        pub fn TessBaseAPIGetTsvText(handle: *mut TessBaseAPI, page_number: c_int) -> *mut c_char;
        pub fn TessBaseAPIClear(handle: *mut TessBaseAPI);
        pub fn TessBaseAPIEnd(handle: *mut TessBaseAPI);
        pub fn TessDeleteText(text: *const c_char);
    }
}

/// A Tesseract engine instance (not thread-safe; create one per worker thread).
#[cfg(feature = "ocr")]
pub struct OcrEngine {
    handle: *mut ffi::TessBaseAPI,
    settings: OcrSettings,
}

#[cfg(feature = "ocr")]
impl OcrEngine {
    pub fn new(settings: OcrSettings) -> anyhow::Result<Self> {
        use std::ffi::CString;
        let handle = unsafe { ffi::TessBaseAPICreate() };
        if handle.is_null() {
            anyhow::bail!("failed to create Tesseract instance");
        }
        let lang = CString::new(settings.language.clone())?;
        let datapath = match &settings.tessdata {
            Some(p) => Some(CString::new(p.to_string_lossy().into_owned())?),
            None => None,
        };
        let rc = unsafe {
            ffi::TessBaseAPIInit3(
                handle,
                datapath.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()),
                lang.as_ptr(),
            )
        };
        if rc != 0 {
            unsafe { ffi::TessBaseAPIDelete(handle) };
            anyhow::bail!(
                "Tesseract could not load language '{}' (install the traineddata or set TESSDATA_PREFIX)",
                settings.language
            );
        }
        unsafe {
            // PSM_AUTO: fully automatic page segmentation without OSD.
            ffi::TessBaseAPISetPageSegMode(handle, 3);
            let name = CString::new("preserve_interword_spaces")?;
            let one = CString::new("1")?;
            ffi::TessBaseAPISetVariable(handle, name.as_ptr(), one.as_ptr());
        }
        Ok(Self { handle, settings })
    }

    pub fn version() -> String {
        unsafe { std::ffi::CStr::from_ptr(ffi::TessVersion()).to_string_lossy().into_owned() }
    }

    pub fn settings(&self) -> &OcrSettings {
        &self.settings
    }

    /// Recognises words on an image rendered at `dpi`; returned boxes are in points.
    pub fn recognize(&mut self, img: &DynamicImage, dpi: f32) -> anyhow::Result<Vec<OcrWord>> {
        let gray = img.to_luma8();
        let (w, h) = gray.dimensions();
        let tsv = unsafe {
            ffi::TessBaseAPISetImage(self.handle, gray.as_raw().as_ptr(), w as i32, h as i32, 1, w as i32);
            ffi::TessBaseAPISetSourceResolution(self.handle, dpi.round() as i32);
            if ffi::TessBaseAPIRecognize(self.handle, std::ptr::null_mut()) != 0 {
                ffi::TessBaseAPIClear(self.handle);
                anyhow::bail!("Tesseract recognition failed");
            }
            let p = ffi::TessBaseAPIGetTsvText(self.handle, 0);
            if p.is_null() {
                ffi::TessBaseAPIClear(self.handle);
                anyhow::bail!("Tesseract returned no result");
            }
            let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
            ffi::TessDeleteText(p);
            ffi::TessBaseAPIClear(self.handle);
            s
        };
        Ok(parse_tsv(&tsv, 72.0 / dpi, self.settings.min_confidence))
    }
}

#[cfg(feature = "ocr")]
impl Drop for OcrEngine {
    fn drop(&mut self) {
        unsafe {
            ffi::TessBaseAPIEnd(self.handle);
            ffi::TessBaseAPIDelete(self.handle);
        }
    }
}

/// Stub used when the crate is built without the `ocr` feature.
#[cfg(not(feature = "ocr"))]
pub struct OcrEngine {
    settings: OcrSettings,
}

#[cfg(not(feature = "ocr"))]
impl OcrEngine {
    pub fn new(_settings: OcrSettings) -> anyhow::Result<Self> {
        anyhow::bail!("this build has no OCR support (rebuild with the `ocr` feature)")
    }
    pub fn version() -> String {
        "unavailable".into()
    }
    pub fn settings(&self) -> &OcrSettings {
        &self.settings
    }
    pub fn recognize(&mut self, _img: &DynamicImage, _dpi: f32) -> anyhow::Result<Vec<OcrWord>> {
        anyhow::bail!("OCR not available")
    }
}

/// Parses Tesseract TSV output (level 5 rows are words).
pub fn parse_tsv(tsv: &str, scale: f32, min_conf: f32) -> Vec<OcrWord> {
    let mut words = Vec::new();
    for line in tsv.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 12 || cols[0] != "5" {
            continue;
        }
        let num = |i: usize| cols[i].trim().parse::<f32>().unwrap_or(0.0);
        let conf = num(10);
        let text = cols[11..].join("\t").trim().to_string();
        if text.is_empty() || conf < 0.0 {
            continue;
        }
        let has_alnum = text.chars().any(|c| c.is_alphanumeric());
        if !has_alnum && conf < min_conf {
            continue;
        }
        if conf < min_conf * 0.4 && text.chars().count() <= 2 {
            continue;
        }
        let (l, t, w, h) = (num(6), num(7), num(8), num(9));
        words.push(OcrWord {
            text,
            bbox: Rect::new(l * scale, t * scale, (l + w) * scale, (t + h) * scale),
            confidence: conf,
            line_key: (num(2) as u32, num(3) as u32, num(4) as u32),
        });
    }
    words
}

const ASCENDERS: &str = "bdfhklt0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ!?%&#$@/\\|[](){}'\"ßÄÖÜÉÈ";
const DESCENDERS: &str = "gjpqy,;()[]{}|/@$Q";

/// Estimates (font size, baseline) of a word from its ink box.
fn word_metrics(word: &OcrWord) -> (f32, f32) {
    let asc = word.text.chars().any(|c| ASCENDERS.contains(c) || c.is_uppercase());
    let desc = word.text.chars().any(|c| DESCENDERS.contains(c));
    let mut em = 0.48;
    if asc {
        em += 0.24;
    }
    if desc {
        em += 0.22;
    }
    let h = word.bbox.height().max(1.0);
    let size = h / em;
    let baseline = if desc { word.bbox.y1 - 0.21 * size } else { word.bbox.y1 };
    (size, baseline)
}

/// Converts OCR words into glyphs and styles on `raw` (replacing existing text).
pub fn words_to_glyphs(raw: &mut RawPage, words: &[OcrWord], img: &DynamicImage, settings: &OcrSettings) {
    raw.glyphs.clear();
    raw.ocr = true;
    if words.is_empty() {
        return;
    }
    let (iw, ih) = img.dimensions();
    let sx = iw as f32 / raw.width;
    let sy = ih as f32 / raw.height;
    let rgb = img.to_rgb8();

    // Ink density and colour per word.
    let mut ink: Vec<(f32, Rgb)> = Vec::with_capacity(words.len());
    for w in words {
        let x0 = ((w.bbox.x0 * sx) as u32).min(iw.saturating_sub(1));
        let y0 = ((w.bbox.y0 * sy) as u32).min(ih.saturating_sub(1));
        let x1 = ((w.bbox.x1 * sx).ceil() as u32).clamp(x0 + 1, iw);
        let y1 = ((w.bbox.y1 * sy).ceil() as u32).clamp(y0 + 1, ih);
        let (mut dark, mut total) = (0u64, 0u64);
        let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
        let step = (((x1 - x0) * (y1 - y0)) as f32 / 40_000.0).sqrt().max(1.0) as usize;
        for y in (y0..y1).step_by(step) {
            for x in (x0..x1).step_by(step) {
                let p = rgb.get_pixel(x, y).0;
                total += 1;
                let lum = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                if lum < 150.0 {
                    dark += 1;
                    r += p[0] as u64;
                    g += p[1] as u64;
                    b += p[2] as u64;
                }
            }
        }
        let density = if total > 0 { dark as f32 / total as f32 } else { 0.0 };
        let color = match (r.checked_div(dark), g.checked_div(dark), b.checked_div(dark)) {
            (Some(r), Some(g), Some(b)) => Rgb(r as u8, g as u8, b as u8).snapped(),
            _ => Rgb::BLACK,
        };
        // Desaturated dark colours are just black ink.
        let color = {
            let mx = color.0.max(color.1).max(color.2) as i32;
            let mn = color.0.min(color.1).min(color.2) as i32;
            if mx - mn < 40 && mx < 110 { Rgb::BLACK } else { color }
        };
        ink.push((density, color));
    }
    let mut densities: Vec<f32> = ink.iter().map(|d| d.0).filter(|d| *d > 0.0).collect();
    densities.sort_by(|a, b| a.total_cmp(b));
    let median_density = densities.get(densities.len() / 2).copied().unwrap_or(0.0);

    // Per-line size/baseline consensus.
    let metrics: Vec<(f32, f32)> = words.iter().map(word_metrics).collect();
    let mut line_groups: std::collections::BTreeMap<(u32, u32, u32), Vec<usize>> = Default::default();
    for (i, w) in words.iter().enumerate() {
        line_groups.entry(w.line_key).or_default().push(i);
    }
    for (_, idxs) in line_groups {
        let mut sizes: Vec<f32> = idxs.iter().map(|&i| metrics[i].0).collect();
        sizes.sort_by(|a, b| a.total_cmp(b));
        let size = sizes[sizes.len() / 2];
        let mut baselines: Vec<f32> = idxs.iter().map(|&i| metrics[i].1).collect();
        baselines.sort_by(|a, b| a.total_cmp(b));
        let baseline = baselines[baselines.len() / 2];
        let size = ((size * 2.0).round() / 2.0).clamp(4.0, 96.0);

        // Bold is decided per line (ink density is too noisy for single short words).
        let line_density = {
            let (mut d, mut n) = (0.0, 0usize);
            for &i in &idxs {
                let len = words[i].text.chars().count();
                d += ink[i].0 * len as f32;
                n += len;
            }
            if n > 0 { d / n as f32 } else { 0.0 }
        };
        let line_bold = median_density > 0.0 && line_density > median_density * 1.3;

        let mut order = idxs.clone();
        order.sort_by(|&a, &b| words[a].bbox.x0.total_cmp(&words[b].bbox.x0));
        for (k, &i) in order.iter().enumerate() {
            let w = &words[i];
            let (density, color) = ink[i];
            let long = w.text.chars().filter(|c| c.is_alphanumeric()).count() >= 5;
            let bold = line_bold || (long && median_density > 0.0 && density > median_density * 1.6);
            let style = raw.style_id(TextStyle {
                font: settings.font.clone(),
                size,
                bold,
                color,
                ..Default::default()
            });
            // Bullets are frequently recognised as "e", "o", "«", "*"...
            let mut text = w.text.clone();
            if k == 0 && order.len() > 1 && text.chars().count() == 1 {
                let next = &words[order[1]];
                let c = text.chars().next().unwrap();
                if "eo°«»*©@¢•·".contains(c) && next.bbox.x0 - w.bbox.x1 > 0.6 * size && w.bbox.height() < 0.75 * size {
                    text = "•".into();
                }
            }
            let chars: Vec<char> = text.chars().collect();
            let n = chars.len().max(1) as f32;
            let cw = w.bbox.width() / n;
            // Words on the same line share its baseline unless clearly off it (outlier boxes are snapped).
            let word_base = if (metrics[i].1 - baseline).abs() < 0.5 * size { baseline } else { metrics[i].1 };
            for (k, &c) in chars.iter().enumerate() {
                let x0 = w.bbox.x0 + cw * k as f32;
                raw.glyphs.push(Glyph {
                    ch: c,
                    bbox: Rect::new(x0, word_base - 0.8 * size, x0 + cw, word_base + 0.22 * size),
                    baseline: word_base,
                    style,
                    space_before: k == 0,
                    link: None,
                });
            }
        }
    }
}

/// Detects thin horizontal and vertical lines (table rulings, separators,
/// underlines) in a rendered page image. Returned rules are in points.
pub fn detect_rules(img: &DynamicImage, dpi: f32, words: &[OcrWord]) -> Vec<Rule> {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let (w, h) = (w as usize, h as usize);
    let dark: Vec<bool> = rgb
        .pixels()
        .map(|p| (0.2126 * p.0[0] as f32 + 0.7152 * p.0[1] as f32 + 0.0722 * p.0[2] as f32) < 140.0)
        .collect();
    let min_len = (dpi * 0.12).max(30.0) as usize; // ~9pt
    let max_thick = (dpi / 40.0).max(4.0) as usize; // ~1.8pt
    let scale = 72.0 / dpi;

    // Generic run extraction along one axis; `at(major, minor)` reads a pixel.
    let extract = |major_len: usize, minor_len: usize, at: &dyn Fn(usize, usize) -> bool| -> Vec<(usize, usize, usize, usize)> {
        // (minor_start, minor_end, major_first, major_last)
        let mut done = Vec::new();
        let mut active: Vec<(usize, usize, usize, usize)> = Vec::new();
        for m in 0..major_len {
            let mut runs = Vec::new();
            let mut start = None;
            for n in 0..=minor_len {
                let d = n < minor_len && at(m, n);
                match (d, start) {
                    (true, None) => start = Some(n),
                    (false, Some(s)) => {
                        if n - s >= min_len {
                            runs.push((s, n));
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
            let mut next_active = Vec::new();
            for (a, b) in runs {
                if let Some(pos) = active.iter().position(|seg| seg.0.abs_diff(a) <= 6 && seg.1.abs_diff(b) <= 6) {
                    let mut seg = active.swap_remove(pos);
                    seg.0 = seg.0.min(a);
                    seg.1 = seg.1.max(b);
                    seg.3 = m;
                    next_active.push(seg);
                } else {
                    next_active.push((a, b, m, m));
                }
            }
            done.append(&mut active);
            active = next_active;
        }
        done.extend(active);
        done.into_iter().filter(|s| s.3 - s.2 < max_thick).collect()
    };

    let mut rules = Vec::new();
    let color_of = |x0: usize, y0: usize, x1: usize, y1: usize| -> Rgb {
        let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
        let step = ((x1 - x0) * (y1 - y0) / 400).max(1);
        let mut k = 0;
        for y in y0..y1.min(h) {
            for x in x0..x1.min(w) {
                k += 1;
                if k % step != 0 || !dark[y * w + x] {
                    continue;
                }
                let p = rgb.get_pixel(x as u32, y as u32).0;
                r += p[0] as u64;
                g += p[1] as u64;
                b += p[2] as u64;
                n += 1;
            }
        }
        match (r.checked_div(n), g.checked_div(n), b.checked_div(n)) {
            (Some(r), Some(g), Some(b)) => Rgb(r as u8, g as u8, b as u8).snapped(),
            _ => Rgb::BLACK,
        }
    };
    let inside_word = |r: &Rect| words.iter().any(|wd| r.fraction_inside(&wd.bbox.expand(0.5)) > 0.6);
    // A real line has light pixels on both sides (edges of filled areas don't).
    let isolated_h = |x0: usize, x1: usize, y0: usize, y1: usize| -> bool {
        let side = |y: Option<usize>| -> f32 {
            let Some(y) = y.filter(|y| *y < h) else { return 0.0 };
            let n = (x0..x1).step_by(3).filter(|&x| dark[y * w + x]).count();
            n as f32 / ((x1 - x0) / 3).max(1) as f32
        };
        side(y0.checked_sub(3)) < 0.5 && side(Some(y1 + 3)) < 0.5
    };
    let isolated_v = |y0: usize, y1: usize, x0: usize, x1: usize| -> bool {
        let side = |x: Option<usize>| -> f32 {
            let Some(x) = x.filter(|x| *x < w) else { return 0.0 };
            let n = (y0..y1).step_by(3).filter(|&y| dark[y * w + x]).count();
            n as f32 / ((y1 - y0) / 3).max(1) as f32
        };
        side(x0.checked_sub(3)) < 0.5 && side(Some(x1 + 3)) < 0.5
    };

    for (x0, x1, y0, y1) in extract(h, w, &|y, x| dark[y * w + x]) {
        if !isolated_h(x0, x1, y0, y1) {
            continue;
        }
        let bbox = Rect::new(x0 as f32 * scale, y0 as f32 * scale, x1 as f32 * scale, (y1 + 1) as f32 * scale);
        if inside_word(&bbox) {
            continue;
        }
        rules.push(Rule {
            bbox,
            orientation: RuleOrientation::Horizontal,
            color: color_of(x0, y0, x1, y1 + 1),
            width: bbox.height().max(0.4),
        });
    }
    for (y0, y1, x0, x1) in extract(w, h, &|x, y| dark[y * w + x]) {
        if !isolated_v(y0, y1, x0, x1) {
            continue;
        }
        let bbox = Rect::new(x0 as f32 * scale, y0 as f32 * scale, (x1 + 1) as f32 * scale, y1 as f32 * scale);
        if inside_word(&bbox) {
            continue;
        }
        rules.push(Rule {
            bbox,
            orientation: RuleOrientation::Vertical,
            color: color_of(x0, y0, x1 + 1, y1),
            width: bbox.width().max(0.4),
        });
    }
    rules
}

/// Finds picture regions (logos, photos, charts) on a scanned page: areas with
/// ink that is neither recognised text nor a ruling line. Returns rectangles in points.
pub fn detect_figures(img: &DynamicImage, dpi: f32, words: &[OcrWord], rules: &[Rule]) -> Vec<Rect> {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let cell = (dpi / 36.0).max(4.0) as u32; // ~2pt
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    let scale = 72.0 / dpi;
    let mut ink = vec![false; (gw * gh) as usize];
    for gy in 0..gh {
        for gx in 0..gw {
            let (mut dark, mut tot) = (0, 0);
            for y in (gy * cell..((gy + 1) * cell).min(h)).step_by(2) {
                for x in (gx * cell..((gx + 1) * cell).min(w)).step_by(2) {
                    let p = rgb.get_pixel(x, y).0;
                    let lum = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                    let sat = p.iter().max().unwrap() - p.iter().min().unwrap();
                    tot += 1;
                    if lum < 180.0 || (sat > 60 && lum < 235.0) {
                        dark += 1;
                    }
                }
            }
            ink[(gy * gw + gx) as usize] = tot > 0 && dark * 10 > tot;
        }
    }
    // Remove text and rules.
    let mut clear = |r: &Rect| {
        let to = |v: f32| (v / scale / cell as f32) as i64;
        for gy in to(r.y0).max(0)..=to(r.y1).min(gh as i64 - 1) {
            for gx in to(r.x0).max(0)..=to(r.x1).min(gw as i64 - 1) {
                ink[(gy as u32 * gw + gx as u32) as usize] = false;
            }
        }
    };
    for wd in words {
        clear(&wd.bbox.expand(1.5));
    }
    for r in rules {
        clear(&r.bbox.expand(1.5));
    }
    // Connected components (with a one-cell bridge).
    let mut seen = vec![false; ink.len()];
    let mut out = Vec::new();
    for start in 0..ink.len() {
        if !ink[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let (mut x0, mut y0, mut x1, mut y1, mut n) = (u32::MAX, u32::MAX, 0u32, 0u32, 0usize);
        while let Some(i) = stack.pop() {
            let (x, y) = (i as u32 % gw, i as u32 / gw);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            n += 1;
            for dy in -2i64..=2 {
                for dx in -2i64..=2 {
                    let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                    if nx < 0 || ny < 0 || nx >= gw as i64 || ny >= gh as i64 {
                        continue;
                    }
                    let j = (ny as u32 * gw + nx as u32) as usize;
                    if ink[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        let r = Rect::new(
            (x0 * cell) as f32 * scale,
            (y0 * cell) as f32 * scale,
            ((x1 + 1) * cell) as f32 * scale,
            ((y1 + 1) * cell) as f32 * scale,
        );
        if n >= 30 && r.width() >= 18.0 && r.height() >= 18.0 && r.area() >= 600.0 {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_parsing() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   1\t1\t0\t0\t0\t0\t0\t0\t2550\t3300\t-1\t\n\
                   5\t1\t1\t1\t1\t1\t300\t300\t150\t50\t96.5\tHello\n\
                   5\t1\t1\t1\t1\t2\t470\t300\t180\t50\t95.0\tworld\n\
                   5\t1\t1\t1\t1\t3\t700\t300\t10\t10\t5.0\t~\n";
        let words = parse_tsv(tsv, 72.0 / 300.0, 30.0);
        assert_eq!(words.len(), 2);
        assert_eq!(words[0].text, "Hello");
        assert!((words[0].bbox.x0 - 72.0).abs() < 1e-3);
        assert_eq!(words[1].line_key, (1, 1, 1));
    }

    #[test]
    fn metrics_estimate() {
        let w = OcrWord { text: "Hello".into(), bbox: Rect::new(0.0, 0.0, 30.0, 8.64), confidence: 90.0, line_key: (1, 1, 1) };
        let (size, base) = word_metrics(&w);
        assert!((size - 12.0).abs() < 0.5, "size {size}");
        assert!((base - 8.64).abs() < 1e-3);
    }
}
