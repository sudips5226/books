//! OCR of rendered pages with Tesseract (via its stable C API).
//!
//! Tesseract returns words with pixel bounding boxes; these are converted into
//! pseudo-glyphs so the regular layout analyser can reconstruct lines,
//! paragraphs, columns and tables exactly as for born-digital PDFs.

use std::collections::HashMap;

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
    /// Individual characters with their boxes (points), when available.
    pub symbols: Vec<(String, Rect)>,
    /// Baseline y of the word's line at the word centre (points), when known.
    pub baseline: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct OcrSettings {
    /// Tesseract language string, e.g. `"eng"` or `"eng+deu"`.
    pub language: String,
    /// Directory containing `*.traineddata` (None = Tesseract default / TESSDATA_PREFIX).
    pub tessdata: Option<std::path::PathBuf>,
    /// Font family used for recognised text; `"auto"` picks Times New Roman,
    /// Arial or Courier New from the shape of the letters.
    pub font: String,
    /// Words below this confidence that contain no alphanumerics are dropped.
    pub min_confidence: f32,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            language: "eng".into(),
            tessdata: None,
            font: "auto".into(),
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
        pub fn TessBaseAPIClear(handle: *mut TessBaseAPI);
        pub fn TessBaseAPIEnd(handle: *mut TessBaseAPI);
        pub fn TessDeleteText(text: *const c_char);
        pub fn TessBaseAPIGetIterator(handle: *mut TessBaseAPI) -> *mut TessResultIterator;
        pub fn TessResultIteratorDelete(handle: *mut TessResultIterator);
        pub fn TessResultIteratorGetPageIterator(handle: *mut TessResultIterator) -> *mut TessPageIterator;
        pub fn TessResultIteratorNext(handle: *mut TessResultIterator, level: c_int) -> c_int;
        pub fn TessResultIteratorGetUTF8Text(handle: *const TessResultIterator, level: c_int) -> *mut c_char;
        pub fn TessResultIteratorConfidence(handle: *const TessResultIterator, level: c_int) -> f32;
        pub fn TessPageIteratorBoundingBox(
            handle: *const TessPageIterator,
            level: c_int,
            left: *mut c_int,
            top: *mut c_int,
            right: *mut c_int,
            bottom: *mut c_int,
        ) -> c_int;
        pub fn TessPageIteratorBaseline(
            handle: *const TessPageIterator,
            level: c_int,
            x1: *mut c_int,
            y1: *mut c_int,
            x2: *mut c_int,
            y2: *mut c_int,
        ) -> c_int;
        pub fn TessPageIteratorIsAtBeginningOf(handle: *const TessPageIterator, level: c_int) -> c_int;
    }

    #[repr(C)]
    pub struct TessResultIterator {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct TessPageIterator {
        _private: [u8; 0],
    }

    pub const RIL_TEXTLINE: c_int = 2;
    pub const RIL_WORD: c_int = 3;
    pub const RIL_SYMBOL: c_int = 4;
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
        let tessdata = settings.tessdata.clone().or_else(bundled_tessdata);
        let datapath = match &tessdata {
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
        use std::os::raw::c_int;
        let gray = img.to_luma8();
        let (w, h) = gray.dimensions();
        let scale = 72.0 / dpi;
        let min_conf = self.settings.min_confidence;
        let mut words: Vec<OcrWord> = Vec::new();
        unsafe {
            ffi::TessBaseAPISetImage(self.handle, gray.as_raw().as_ptr(), w as i32, h as i32, 1, w as i32);
            ffi::TessBaseAPISetSourceResolution(self.handle, dpi.round() as i32);
            if ffi::TessBaseAPIRecognize(self.handle, std::ptr::null_mut()) != 0 {
                ffi::TessBaseAPIClear(self.handle);
                anyhow::bail!("Tesseract recognition failed");
            }
            let it = ffi::TessBaseAPIGetIterator(self.handle);
            if it.is_null() {
                ffi::TessBaseAPIClear(self.handle);
                return Ok(words);
            }
            let pit = ffi::TessResultIteratorGetPageIterator(it);
            let text_at = |level: c_int| -> String {
                let p = ffi::TessResultIteratorGetUTF8Text(it, level);
                if p.is_null() {
                    return String::new();
                }
                let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
                ffi::TessDeleteText(p);
                s
            };
            let bbox_at = |level: c_int| -> Option<Rect> {
                let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
                (ffi::TessPageIteratorBoundingBox(pit, level, &mut l, &mut t, &mut r, &mut b) != 0)
                    .then(|| Rect::new(l as f32 * scale, t as f32 * scale, r as f32 * scale, b as f32 * scale))
            };
            let mut line_no = 0u32;
            let mut line_base: Option<(f32, f32, f32, f32)> = None;
            let mut current: Option<OcrWord> = None;
            loop {
                if ffi::TessPageIteratorIsAtBeginningOf(pit, ffi::RIL_TEXTLINE) != 0 {
                    line_no += 1;
                    let (mut x1, mut y1, mut x2, mut y2) = (0, 0, 0, 0);
                    line_base = (ffi::TessPageIteratorBaseline(pit, ffi::RIL_TEXTLINE, &mut x1, &mut y1, &mut x2, &mut y2) != 0)
                        .then(|| (x1 as f32 * scale, y1 as f32 * scale, x2 as f32 * scale, y2 as f32 * scale));
                }
                if ffi::TessPageIteratorIsAtBeginningOf(pit, ffi::RIL_WORD) != 0 {
                    if let Some(wd) = current.take() {
                        words.push(wd);
                    }
                    if let Some(bbox) = bbox_at(ffi::RIL_WORD) {
                        let baseline = line_base.map(|(x1, y1, x2, y2)| {
                            let cx = bbox.cx();
                            if (x2 - x1).abs() < 1e-3 { y1 } else { y1 + (y2 - y1) * (cx - x1) / (x2 - x1) }
                        });
                        current = Some(OcrWord {
                            text: text_at(ffi::RIL_WORD).trim().to_string(),
                            bbox,
                            confidence: ffi::TessResultIteratorConfidence(it, ffi::RIL_WORD),
                            line_key: (0, 0, line_no),
                            symbols: Vec::new(),
                            baseline,
                        });
                    }
                }
                if let (Some(wd), Some(b)) = (current.as_mut(), bbox_at(ffi::RIL_SYMBOL)) {
                    let t = text_at(ffi::RIL_SYMBOL);
                    if !t.trim().is_empty() {
                        wd.symbols.push((t, b));
                    }
                }
                if ffi::TessResultIteratorNext(it, ffi::RIL_SYMBOL) == 0 {
                    break;
                }
            }
            if let Some(wd) = current.take() {
                words.push(wd);
            }
            ffi::TessResultIteratorDelete(it);
            ffi::TessBaseAPIClear(self.handle);
        }
        // Same filtering as for TSV output.
        words.retain(|w| {
            let has_alnum = w.text.chars().any(|c| c.is_alphanumeric());
            !w.text.is_empty() && w.confidence >= 0.0 && (has_alnum || w.confidence >= min_conf) && !(w.confidence < min_conf * 0.4 && w.text.chars().count() <= 2)
        });
        Ok(words)
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

/// A `tessdata` folder shipped next to the executable (portable Windows/macOS
/// bundles), used unless `TESSDATA_PREFIX` is set.
#[cfg_attr(not(feature = "ocr"), allow(dead_code))]
fn bundled_tessdata() -> Option<std::path::PathBuf> {
    if std::env::var_os("TESSDATA_PREFIX").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    // Windows/portable: next to the exe; Linux AppImage: ../share; macOS bundle: ../Resources.
    [dir.join("tessdata"), dir.join("../share/tessdata"), dir.join("../Resources/tessdata")].into_iter().find(|p| p.is_dir())
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
            symbols: Vec::new(),
            baseline: None,
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

/// Shape measurements of one recognised word.
#[derive(Clone, Copy, Debug, Default)]
struct WordShape {
    /// Median horizontal stroke run divided by the font size in pixels.
    stroke: f32,
    /// Shear (dx per dy) that makes vertical strokes sharpest; italic ≈ 0.2.
    slant: f32,
    /// Ratio of ink run lengths at the foot of letters vs mid-height (serifs).
    foot: f32,
    /// Coefficient of variation of character pitch (monospace ≈ 0).
    pitch_cv: Option<f32>,
    /// Number of letters (measurements of short words are unreliable).
    letters: usize,
}

/// Letters whose height is the x-height without round overshoot.
const XHEIGHT_LETTERS: &str = "xzvwnmru";
/// Capitals with flat (or pointed) tops and bottoms.
const FLAT_CAPS: &str = "ABDEFHIKLMNPRTUVWXYZ";

/// Median cap height and x-height of a line's characters (points, blur-corrected) and its baseline.
fn line_heights(words: &[&OcrWord], blur: f32) -> (Option<f32>, Option<f32>, f32) {
    let mut caps = Vec::new();
    let mut xs = Vec::new();
    for w in words {
        for (t, b) in &w.symbols {
            let mut it = t.chars();
            let (Some(c), None) = (it.next(), it.next()) else { continue };
            if FLAT_CAPS.contains(c) {
                caps.push(b.height() - blur);
            } else if XHEIGHT_LETTERS.contains(c) {
                xs.push(b.height() - blur);
            }
        }
    }
    let med = |v: &mut Vec<f32>| -> Option<f32> {
        if v.is_empty() {
            return None;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        Some(v[v.len() / 2])
    };
    let cap = med(&mut caps);
    // Lower quartile: blur and stray overshoots only ever make boxes taller.
    let xh = if xs.len() >= 2 {
        xs.sort_by(|a, b| a.total_cmp(b));
        Some(xs[xs.len() / 4])
    } else {
        None
    };
    let mut bases: Vec<f32> = words.iter().map(|w| w.baseline.unwrap_or_else(|| word_metrics(w).1)).collect();
    (cap, xh, med(&mut bases).unwrap_or(0.0))
}

/// Font size from cap height / x-height using the proportions of the font class.
fn size_from_heights(cap: Option<f32>, xh: Option<f32>, class: Generic, words: &[&OcrWord]) -> f32 {
    let (c, x) = match class {
        Generic::Serif => (0.662, 0.448),
        Generic::Sans => (0.716, 0.519),
        Generic::Mono => (0.571, 0.423),
        Generic::Unknown => (0.69, 0.48),
    };
    match (cap.map(|v| v / c), xh.map(|v| v / x)) {
        (Some(a), _) => a,
        (None, Some(b)) => b,
        (None, None) => {
            let mut v: Vec<f32> = words.iter().map(|w| word_metrics(w).0).collect();
            v.sort_by(|a, b| a.total_cmp(b));
            v.get(v.len() / 2).copied().unwrap_or(11.0)
        }
    }
}

/// Measures stroke width, slant, serifs and character pitch of a word.
fn word_shape(word: &OcrWord, ink: &image::GrayImage, scale: f32, size_px: f32, base_px: f32) -> WordShape {
    let (iw, ih) = ink.dimensions();
    let x0 = ((word.bbox.x0 * scale).floor().max(0.0) as u32).min(iw.saturating_sub(1));
    let x1 = ((word.bbox.x1 * scale).ceil() as u32).clamp(x0 + 1, iw);
    let y0 = ((word.bbox.y0 * scale).floor().max(0.0) as u32).min(ih.saturating_sub(1));
    let y1 = ((word.bbox.y1 * scale).ceil() as u32).clamp(y0 + 1, ih);
    let dark = |x: u32, y: u32| ink.get_pixel(x, y).0[0] < 128;
    let letters = word.text.chars().filter(|c| c.is_alphanumeric()).count();
    let mut shape = WordShape { letters, ..Default::default() };
    if size_px < 6.0 || x1 - x0 < 3 {
        return shape;
    }
    // Horizontal ink runs per row.
    let runs_in_row = |y: u32| -> Vec<u32> {
        let mut v = Vec::new();
        let mut run = 0;
        for x in x0..x1 {
            if dark(x, y) {
                run += 1;
            } else if run > 0 {
                v.push(run);
                run = 0;
            }
        }
        if run > 0 {
            v.push(run);
        }
        v
    };
    let xh = 0.48 * size_px;
    let base = base_px.clamp(y0 as f32, y1 as f32);
    let mid_lo = (base - 0.75 * xh).max(y0 as f32) as u32;
    let mid_hi = (base - 0.25 * xh).max(y0 as f32 + 1.0) as u32;
    let mut mid_runs: Vec<u32> = (mid_lo..mid_hi.min(y1)).flat_map(runs_in_row).filter(|r| (*r as f32) < 0.5 * size_px).collect();
    let med = |v: &mut Vec<u32>| -> Option<f32> {
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[v.len() / 2] as f32)
    };
    if let Some(m) = med(&mut mid_runs) {
        shape.stroke = m / size_px;
    }
    // Serifs: under straight-stemmed letters the ink at the very foot is much
    // wider than the stem itself.
    let mut ratios = Vec::new();
    for (t, b) in &word.symbols {
        let mut it = t.chars();
        let (Some(c), None) = (it.next(), it.next()) else { continue };
        if !"ilnmhrIHLNMkK".contains(c) {
            continue;
        }
        let sx0 = ((b.x0 * scale).floor().max(0.0) as u32).max(x0);
        let sx1 = ((b.x1 * scale).ceil() as u32).min(x1);
        if sx1 <= sx0 + 1 {
            continue;
        }
        let row_runs = |y: u32| -> Vec<u32> {
            let mut v = Vec::new();
            let mut run = 0;
            for x in sx0..sx1 {
                if dark(x, y) {
                    run += 1;
                } else if run > 0 {
                    v.push(run);
                    run = 0;
                }
            }
            if run > 0 {
                v.push(run);
            }
            v
        };
        let mid_y = ((base - 0.25 * size_px) as u32).clamp(y0, y1 - 1);
        let mut stem = row_runs(mid_y);
        let Some(stem_w) = med(&mut stem) else { continue };
        // Look at the lowest rows above the baseline (the baseline estimate is ±1px).
        let foot = (1..=3)
            .filter_map(|d| {
                let y = (base.round() as i64 - d).clamp(y0 as i64, y1 as i64 - 1) as u32;
                let mut r = row_runs(y);
                med(&mut r)
            })
            .fold(0.0f32, f32::max);
        if stem_w > 0.0 {
            ratios.push(foot / stem_w);
        }
    }
    if !ratios.is_empty() {
        ratios.sort_by(|a, b| a.total_cmp(b));
        shape.foot = ratios[ratios.len() / 2];
    }
    // Slant: the shear that maximises the sharpness of the column histogram.
    let ys: Vec<u32> = (y0..y1).filter(|y| (*y as f32) <= base + 1.0).collect();
    let score = |s: f32| -> f32 {
        let mut cols = vec![0u32; (x1 - x0) as usize + (size_px as usize) + 2];
        for &y in &ys {
            let shift = (s * (base - y as f32)).round() as i64;
            for x in x0..x1 {
                if dark(x, y) {
                    let c = (x - x0) as i64 - shift + (0.5 * size_px) as i64;
                    if c >= 0 && (c as usize) < cols.len() {
                        cols[c as usize] += 1;
                    }
                }
            }
        }
        cols.iter().map(|c| (*c as f32).powi(2)).sum()
    };
    let upright = score(0.0);
    let mut best = (0.0f32, upright);
    for k in 1..=9 {
        let s = k as f32 * 0.05;
        let v = score(s);
        if v > best.1 {
            best = (s, v);
        }
    }
    if best.1 > upright * 1.06 {
        shape.slant = best.0;
    }
    // Character pitch regularity (monospace fonts).
    if word.symbols.len() >= 4 {
        let centers: Vec<f32> = word.symbols.iter().map(|(_, b)| b.cx()).collect();
        let d: Vec<f32> = centers.windows(2).map(|w| w[1] - w[0]).collect();
        let mean = d.iter().sum::<f32>() / d.len() as f32;
        if mean > 0.0 {
            let var = d.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / d.len() as f32;
            shape.pitch_cv = Some(var.sqrt() / mean);
        }
    }
    shape
}

/// Splits values into two classes (Otsu); returns a threshold if they are clearly bimodal.
fn bimodal_threshold(values: &[f32], min_ratio: f32) -> Option<f32> {
    if values.len() < 4 {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    let total: f32 = v.iter().sum();
    let mut best: Option<(f32, usize)> = None;
    let mut left = 0.0;
    for i in 1..n {
        left += v[i - 1];
        let (w0, w1) = (i as f32, (n - i) as f32);
        let (m0, m1) = (left / w0, (total - left) / w1);
        let between = w0 * w1 * (m0 - m1).powi(2);
        if best.is_none_or(|b| between > b.0) {
            best = Some((between, i));
        }
    }
    let (_, i) = best?;
    let m0 = v[..i].iter().sum::<f32>() / i as f32;
    let m1 = v[i..].iter().sum::<f32>() / (n - i) as f32;
    (m0 > 0.0 && m1 / m0 >= min_ratio).then(|| (v[i - 1] + v[i]) / 2.0)
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
    let gray = img.to_luma8();

    // Colour per word: average of its dark pixels.
    let colors: Vec<Rgb> = words
        .iter()
        .map(|w| {
            let x0 = ((w.bbox.x0 * sx) as u32).min(iw.saturating_sub(1));
            let y0 = ((w.bbox.y0 * sy) as u32).min(ih.saturating_sub(1));
            let x1 = ((w.bbox.x1 * sx).ceil() as u32).clamp(x0 + 1, iw);
            let y1 = ((w.bbox.y1 * sy).ceil() as u32).clamp(y0 + 1, ih);
            let (mut n, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
            let step = (((x1 - x0) * (y1 - y0)) as f32 / 40_000.0).sqrt().max(1.0) as usize;
            for y in (y0..y1).step_by(step) {
                for x in (x0..x1).step_by(step) {
                    let p = rgb.get_pixel(x, y).0;
                    if 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32 > 150.0 {
                        continue;
                    }
                    n += 1;
                    r += p[0] as u64;
                    g += p[1] as u64;
                    b += p[2] as u64;
                }
            }
            if n == 0 {
                return Rgb::BLACK;
            }
            let c = Rgb((r / n) as u8, (g / n) as u8, (b / n) as u8).snapped();
            let mx = c.0.max(c.1).max(c.2) as i32;
            let mn = c.0.min(c.1).min(c.2) as i32;
            if mx - mn < 40 && mx < 110 { Rgb::BLACK } else { c }
        })
        .collect();

    // Lines → metrics → shapes.
    let mut line_groups: std::collections::BTreeMap<(u32, u32, u32), Vec<usize>> = Default::default();
    for (i, w) in words.iter().enumerate() {
        line_groups.entry(w.line_key).or_default().push(i);
    }
    let mut metrics: Vec<(f32, f32)> = vec![(11.0, 0.0); words.len()];
    let mut shapes: Vec<WordShape> = vec![WordShape::default(); words.len()];
    let blur = 1.0 / sx;
    let mut line_info: std::collections::BTreeMap<(u32, u32, u32), (Option<f32>, Option<f32>)> = Default::default();
    for (key, idxs) in &line_groups {
        let ws: Vec<&OcrWord> = idxs.iter().map(|&i| &words[i]).collect();
        let (cap, xh, base) = line_heights(&ws, blur);
        line_info.insert(*key, (cap, xh));
        let size = size_from_heights(cap, xh, Generic::Unknown, &ws);
        let size = ((size * 2.0).round() / 2.0).clamp(4.0, 96.0);
        for &i in idxs {
            // Words clearly off the line baseline (outlier boxes) keep their own.
            let own = words[i].baseline.unwrap_or_else(|| word_metrics(&words[i]).1);
            let b = if (own - base).abs() < 0.5 * size { base } else { own };
            metrics[i] = (size, b);
            shapes[i] = word_shape(&words[i], &gray, sx, size * sx, b * sy);
        }
    }

    // Font class per line (shape of serifs and character pitch).
    let reliable_of = |idxs: &Vec<usize>| -> Vec<usize> {
        idxs.iter().copied().filter(|&i| shapes[i].letters >= 3 && shapes[i].stroke > 0.0).collect()
    };
    let mut line_class: std::collections::BTreeMap<(u32, u32, u32), Generic> = Default::default();
    for (key, idxs) in &line_groups {
        let footed: Vec<f32> = idxs.iter().map(|&i| shapes[i].foot).filter(|f| *f > 0.0).collect();
        let serif = !footed.is_empty() && footed.iter().filter(|f| **f >= 1.45).count() * 2 >= footed.len();
        let pitches: Vec<f32> = idxs.iter().filter_map(|&i| shapes[i].pitch_cv).collect();
        let mono = !pitches.is_empty() && pitches.iter().filter(|cv| **cv < 0.1).count() * 10 >= pitches.len() * 7;
        line_class.insert(*key, if mono { Generic::Mono } else if serif { Generic::Serif } else { Generic::Sans });
    }

    // Bold: stroke width relative to other text of the same font class (light
    // fonts such as Courier have thin bold strokes), else page-wide.
    let threshold_of = |values: &[f32]| -> Option<f32> { bimodal_threshold(values, 1.3) };
    let all: Vec<f32> = line_groups.values().flat_map(|v| reliable_of(v)).map(|i| shapes[i].stroke).collect();
    let page_threshold = threshold_of(&all).unwrap_or_else(|| {
        let mut v = all.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        v.get(v.len() / 2).copied().unwrap_or(0.0) * 1.45
    });
    let mut class_threshold: HashMap<Generic, f32> = HashMap::new();
    for class in [Generic::Serif, Generic::Sans, Generic::Mono] {
        let vals: Vec<f32> = line_groups
            .iter()
            .filter(|(k, _)| line_class[k] == class)
            .flat_map(|(_, v)| reliable_of(v))
            .map(|i| shapes[i].stroke)
            .collect();
        if let Some(t) = threshold_of(&vals) {
            class_threshold.insert(class, t);
        }
    }

    for (key, idxs) in &line_groups {
        let reliable = reliable_of(idxs);
        let frac = |f: &dyn Fn(&WordShape) -> bool| -> f32 {
            if reliable.is_empty() {
                0.0
            } else {
                reliable.iter().filter(|&&i| f(&shapes[i])).count() as f32 / reliable.len() as f32
            }
        };
        let class = line_class[key];
        let bold_threshold = class_threshold.get(&class).copied().unwrap_or(page_threshold);
        let line_bold = bold_threshold > 0.0 && frac(&|s| s.stroke > bold_threshold) >= 0.6;
        let line_italic = frac(&|s| s.slant >= 0.15) >= 0.6;
        let (font, generic) = if settings.font.eq_ignore_ascii_case("auto") || settings.font.is_empty() {
            match class {
                Generic::Mono => ("Courier New".to_string(), Generic::Mono),
                Generic::Serif => ("Times New Roman".to_string(), Generic::Serif),
                _ => ("Arial".to_string(), Generic::Sans),
            }
        } else {
            (settings.font.clone(), crate::fonts::generic_of(&settings.font))
        };

        // Refine the size with the proportions of the detected font class.
        let ws: Vec<&OcrWord> = idxs.iter().map(|&i| &words[i]).collect();
        let (cap, xh) = line_info[key];
        let line_size = ((size_from_heights(cap, xh, generic, &ws) * 2.0).round() / 2.0).clamp(4.0, 96.0);
        log::trace!("ocr line {key:?} cap={cap:?} xh={xh:?} class={generic:?} size={line_size}");

        let mut order = idxs.clone();
        order.sort_by(|&a, &b| words[a].bbox.x0.total_cmp(&words[b].bbox.x0));
        for (k, &i) in order.iter().enumerate() {
            let w = &words[i];
            let (_, base) = metrics[i];
            let size = line_size;
            let sh = &shapes[i];
            log::trace!("ocr word {:?} size={size} {:?}", w.text, sh);
            let long = sh.letters >= 4;
            let bold = line_bold || (long && bold_threshold > 0.0 && sh.stroke > bold_threshold * 1.1);
            let italic = line_italic || (long && sh.slant >= 0.15);
            let style = raw.style_id(TextStyle {
                font: font.clone(),
                generic,
                size,
                bold,
                italic,
                color: colors[i],
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
            // Character boxes: Tesseract's symbols when they line up with the text, else even split.
            let chars: Vec<char> = text.chars().collect();
            let boxes: Vec<(f32, f32)> = if w.symbols.len() == chars.len() && text == w.text {
                let n = w.symbols.len();
                (0..n)
                    .map(|j| {
                        let b = w.symbols[j].1;
                        // Advance ≈ distance to the next character (ink box + side bearings).
                        let x1 = if j + 1 < n { w.symbols[j + 1].1.x0 } else { b.x1 + 0.04 * size };
                        (if j == 0 { b.x0 - 0.04 * size } else { b.x0 }, x1.max(b.x1))
                    })
                    .collect()
            } else {
                let n = chars.len().max(1) as f32;
                let cw = w.bbox.width() / n;
                (0..chars.len()).map(|j| (w.bbox.x0 + cw * j as f32, w.bbox.x0 + cw * (j + 1) as f32)).collect()
            };
            for (j, &c) in chars.iter().enumerate() {
                let (x0, x1) = boxes[j];
                raw.glyphs.push(Glyph {
                    ch: c,
                    bbox: Rect::new(x0, base - 0.8 * size, x1, base + 0.22 * size),
                    baseline: base,
                    style,
                    space_before: j == 0,
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
        let w = OcrWord { text: "Hello".into(), bbox: Rect::new(0.0, 0.0, 30.0, 8.64), confidence: 90.0, line_key: (1, 1, 1), symbols: vec![], baseline: None };
        let (size, base) = word_metrics(&w);
        assert!((size - 12.0).abs() < 0.5, "size {size}");
        assert!((base - 8.64).abs() < 1e-3);
    }
}
