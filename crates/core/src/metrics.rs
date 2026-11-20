//! Font metrics and width compensation.
//!
//! Word lays text out with whatever font it has for a family name. When that
//! font is wider or narrower than the one used in the PDF, lines break in
//! different places and pages overflow. For every text run we know its exact
//! width in the PDF; here we measure the same text with the font Word will use
//! and add character spacing (small differences) or horizontal scaling (large
//! differences) so the run occupies its original width again.
//!
//! Widths are resolved in this order:
//! 1. fonts embedded into the DOCX from the PDF ([`FontLibrary::register`]),
//! 2. a built-in table for Word's core fonts (Times New Roman, Arial, Courier
//!    New, Calibri, Cambria — from their metric-compatible open clones),
//! 3. installed system fonts (scanned once, lazily),
//! 4. the generic substitute Word would pick (serif → Times New Roman,
//!    sans → Arial, monospace → Courier New).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::model::*;

static BUILTIN_DATA: &[u8] = include_bytes!("../data/metrics.bin");

struct TableFace {
    name: String,
    bold: bool,
    italic: bool,
    upem: f32,
    default: u16,
    widths: HashMap<u32, u16>,
}

fn builtin_faces() -> &'static [TableFace] {
    static FACES: OnceLock<Vec<TableFace>> = OnceLock::new();
    FACES.get_or_init(|| parse_builtin(BUILTIN_DATA).unwrap_or_default())
}

fn parse_builtin(d: &[u8]) -> Option<Vec<TableFace>> {
    let rd16 = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?)) };
    let rd32 = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    if d.get(0..4)? != b"P2WM" || rd16(4)? != 1 {
        return None;
    }
    let n = rd16(6)? as usize;
    let mut o = 8;
    let mut faces = Vec::with_capacity(n);
    for _ in 0..n {
        let len = *d.get(o)? as usize;
        let name = String::from_utf8(d.get(o + 1..o + 1 + len)?.to_vec()).ok()?;
        o += 1 + len;
        let style = *d.get(o)?;
        let upem = rd16(o + 1)? as f32;
        let default = rd16(o + 3)?;
        let count = rd32(o + 5)? as usize;
        o += 9;
        let mut widths = HashMap::with_capacity(count);
        for _ in 0..count {
            widths.insert(rd32(o)?, rd16(o + 4)?);
            o += 6;
        }
        faces.push(TableFace { name, bold: style & 1 != 0, italic: style & 2 != 0, upem, default, widths });
    }
    Some(faces)
}

/// Names under which the built-in families are also known.
fn builtin_alias(family: &str) -> Option<&'static str> {
    let f = family.to_ascii_lowercase();
    Some(match f.as_str() {
        "times new roman" | "times" | "liberation serif" | "tinos" | "nimbus roman" | "tex gyre termes" => "Times New Roman",
        "arial" | "helvetica" | "liberation sans" | "arimo" | "nimbus sans" | "tex gyre heros" => "Arial",
        "courier new" | "courier" | "liberation mono" | "cousine" | "nimbus mono" => "Courier New",
        "calibri" | "carlito" => "Calibri",
        "cambria" | "caladea" => "Cambria",
        _ => return None,
    })
}

/// Parsed font data (TrueType/OpenType) with a width cache.
pub struct FontData {
    bytes: Vec<u8>,
    index: u32,
    upem: f32,
    cache: Mutex<HashMap<char, Option<f32>>>,
}

impl FontData {
    pub fn new(bytes: Vec<u8>, index: u32) -> Option<Self> {
        let face = ttf_parser::Face::parse(&bytes, index).ok()?;
        let upem = face.units_per_em() as f32;
        Some(Self { bytes, index, upem, cache: Mutex::new(HashMap::new()) })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Advance width of `c` in em, if the font has a glyph for it.
    pub fn advance(&self, c: char) -> Option<f32> {
        if let Some(v) = self.cache.lock().ok()?.get(&c) {
            return *v;
        }
        let v = ttf_parser::Face::parse(&self.bytes, self.index).ok().and_then(|f| {
            let g = f.glyph_index(c)?;
            Some(f.glyph_hor_advance(g)? as f32 / self.upem)
        });
        if let Ok(mut c2) = self.cache.lock() {
            c2.insert(c, v);
        }
        v
    }

    /// True if every character of `text` (except spaces) has a glyph.
    pub fn covers(&self, text: &str) -> bool {
        let Ok(f) = ttf_parser::Face::parse(&self.bytes, self.index) else { return false };
        text.chars().filter(|c| !c.is_whitespace()).all(|c| f.glyph_index(c).is_some_and(|g| g.0 != 0))
    }
}

#[derive(Clone)]
enum Resolved {
    Table(usize),
    Data(Arc<FontData>),
}

/// Resolves font families to widths.
pub struct FontLibrary {
    registered: HashMap<(String, bool, bool), Arc<FontData>>,
    use_system: bool,
    cache: Mutex<HashMap<(String, bool, bool), Option<Resolved>>>,
}

impl Default for FontLibrary {
    fn default() -> Self {
        Self::new(true)
    }
}

impl FontLibrary {
    pub fn new(use_system: bool) -> Self {
        Self { registered: HashMap::new(), use_system, cache: Mutex::new(HashMap::new()) }
    }

    /// Registers a font that Word will have (e.g. one embedded into the DOCX).
    pub fn register(&mut self, family: &str, bold: bool, italic: bool, font: Arc<FontData>) {
        self.registered.insert((family.to_ascii_lowercase(), bold, italic), font);
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }

    fn resolve(&self, family: &str, bold: bool, italic: bool) -> Option<Resolved> {
        let key = (family.to_ascii_lowercase(), bold, italic);
        if let Some(r) = self.cache.lock().ok()?.get(&key) {
            return r.clone();
        }
        let r = self.resolve_uncached(&key.0, bold, italic);
        if let Ok(mut c) = self.cache.lock() {
            c.insert(key, r.clone());
        }
        r
    }

    fn resolve_uncached(&self, family: &str, bold: bool, italic: bool) -> Option<Resolved> {
        for (b, i) in [(bold, italic), (bold, false), (false, italic), (false, false)] {
            if let Some(f) = self.registered.get(&(family.to_string(), b, i)) {
                return Some(Resolved::Data(f.clone()));
            }
        }
        if let Some(alias) = builtin_alias(family) {
            let faces = builtin_faces();
            if let Some(i) = faces.iter().position(|f| f.name == alias && f.bold == bold && f.italic == italic) {
                return Some(Resolved::Table(i));
            }
        }
        if self.use_system {
            if let Some(f) = system_font(family, bold, italic) {
                return Some(Resolved::Data(f));
            }
        }
        None
    }

    /// The family whose metrics Word will effectively use for `style`.
    fn effective(&self, style: &TextStyle) -> Option<Resolved> {
        self.resolve(&style.font, style.bold, style.italic).or_else(|| {
            let fallback = match style.generic {
                Generic::Sans => "Arial",
                Generic::Mono => "Courier New",
                _ => "Times New Roman",
            };
            self.resolve(fallback, style.bold, style.italic)
        })
    }

    /// Width of `text` in points with the font Word will use, and the number
    /// of characters whose width is unknown.
    pub fn measure(&self, text: &str, style: &TextStyle) -> Option<(f32, usize)> {
        let face = self.effective(style)?;
        let mut w = 0.0;
        let mut unknown = 0;
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            let adv = match &face {
                Resolved::Table(i) => {
                    let f = &builtin_faces()[*i];
                    match f.widths.get(&(c as u32)) {
                        Some(v) => Some(*v as f32 / f.upem),
                        None => {
                            unknown += 1;
                            Some(f.default as f32 / f.upem)
                        }
                    }
                }
                Resolved::Data(d) => d.advance(c).or_else(|| {
                    unknown += 1;
                    Some(0.5)
                }),
            };
            w += adv.unwrap_or(0.5) * style.size;
        }
        Some((w, unknown))
    }
}

// ---------------------------------------------------------------------------
// System fonts
// ---------------------------------------------------------------------------

fn font_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
    if cfg!(windows) {
        let windir = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        v.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        v.push("/System/Library/Fonts".into());
        v.push("/Library/Fonts".into());
        if let Some(h) = &home {
            v.push(h.join("Library/Fonts"));
        }
    } else {
        v.push("/usr/share/fonts".into());
        v.push("/usr/local/share/fonts".into());
        if let Some(h) = &home {
            v.push(h.join(".local/share/fonts"));
            v.push(h.join(".fonts"));
        }
    }
    v
}

type SystemIndex = HashMap<(String, bool, bool), (PathBuf, u32)>;

fn system_index() -> &'static SystemIndex {
    static INDEX: OnceLock<SystemIndex> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut idx = SystemIndex::new();
        for d in font_dirs() {
            scan_dir(&d, 0, &mut idx);
        }
        log::debug!("indexed {} system font faces", idx.len());
        idx
    })
}

fn scan_dir(dir: &Path, depth: usize, idx: &mut SystemIndex) {
    if depth > 5 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_dir(&p, depth + 1, idx);
            continue;
        }
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if !matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
            continue;
        }
        if e.metadata().map(|m| m.len() > 80 << 20).unwrap_or(true) {
            continue;
        }
        let Ok(data) = std::fs::read(&p) else { continue };
        let n = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        for i in 0..n {
            let Ok(face) = ttf_parser::Face::parse(&data, i) else { continue };
            let (bold, italic) = (face.is_bold(), face.is_italic());
            for family in face_families(&face) {
                idx.entry((family.to_ascii_lowercase(), bold, italic)).or_insert_with(|| (p.clone(), i));
            }
        }
    }
}

/// Family names of a face (typographic and legacy, English preferred).
pub fn face_families(face: &ttf_parser::Face) -> Vec<String> {
    let mut v = Vec::new();
    for id in [ttf_parser::name_id::TYPOGRAPHIC_FAMILY, ttf_parser::name_id::FAMILY] {
        for n in face.names() {
            if n.name_id == id {
                if let Some(s) = n.to_string() {
                    if !s.is_empty() && !v.contains(&s) {
                        v.push(s);
                    }
                }
            }
        }
    }
    v
}

fn system_font(family: &str, bold: bool, italic: bool) -> Option<Arc<FontData>> {
    type Loaded = Mutex<HashMap<(PathBuf, u32), Arc<FontData>>>;
    static LOADED: OnceLock<Loaded> = OnceLock::new();
    let idx = system_index();
    let key = [(bold, italic), (bold, false), (false, italic), (false, false)]
        .into_iter()
        .find_map(|(b, i)| idx.get(&(family.to_ascii_lowercase(), b, i)))?;
    let loaded = LOADED.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = loaded.lock().ok()?;
    if let Some(f) = map.get(key) {
        return Some(f.clone());
    }
    let data = std::fs::read(&key.0).ok()?;
    let f = Arc::new(FontData::new(data, key.1)?);
    map.insert(key.clone(), f.clone());
    Some(f)
}

// ---------------------------------------------------------------------------
// Compensation
// ---------------------------------------------------------------------------

/// Width compensation for loose blocks (headers, footers, footnotes).
pub fn compensate_blocks(blocks: &mut [Block], lib: &FontLibrary) {
    let mut page = PageLayout { blocks: std::mem::take(&mut blocks.to_vec()), ..Default::default() };
    compensate(std::slice::from_mut(&mut page), lib);
    for (dst, src) in blocks.iter_mut().zip(page.blocks) {
        *dst = src;
    }
}

/// Adjusts character spacing / scaling of every run so that it keeps its PDF width.
pub fn compensate(pages: &mut [PageLayout], lib: &FontLibrary) {
    fn inlines(v: &mut [Inline], lib: &FontLibrary) {
        for i in v {
            if let Inline::Text { text, style, pdf_width, pdf_tracking, .. } = i {
                adjust(text, style, *pdf_width, *pdf_tracking, lib);
            }
        }
    }
    fn blocks(v: &mut [Block], lib: &FontLibrary) {
        for b in v {
            match b {
                Block::Paragraph(p) => inlines(&mut p.inlines, lib),
                Block::Table(t) => {
                    for r in &mut t.rows {
                        for c in &mut r.cells {
                            blocks(&mut c.blocks, lib);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for p in pages {
        blocks(&mut p.blocks, lib);
        for tb in &mut p.text_boxes {
            inlines(&mut tb.inlines, lib);
        }
    }
}

/// Scripts whose glyphs are shaped (joined, reordered, combined): their
/// width cannot be predicted from per-character advances.
fn needs_shaping(c: char) -> bool {
    matches!(c as u32,
        0x0591..=0x08FF | 0x0900..=0x0DFF | 0x0E00..=0x0FFF | 0x1000..=0x109F | 0x1780..=0x17FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

fn adjust(text: &str, style: &mut TextStyle, pdf_width: f32, pdf_tracking: f32, lib: &FontLibrary) {
    let n = text.chars().filter(|c| !c.is_whitespace()).count();
    if pdf_width <= 0.0 || n < 2 || style.small_caps || text.chars().any(needs_shaping) {
        return;
    }
    let Some((word_w, unknown)) = lib.measure(text, style) else { return };
    if word_w <= 0.0 || unknown * 4 > n {
        return;
    }
    let ratio = pdf_width / word_w;
    if (ratio - 1.0).abs() < 0.015 {
        return;
    }
    // Letter spacing in the PDF stays letter spacing; only differences in the
    // glyphs themselves are worth scaling.
    let glyph_ratio = (pdf_width - pdf_tracking) / word_w;
    let (scale, rest) = if (0.92..=1.08).contains(&glyph_ratio) {
        (100u16, pdf_width - word_w)
    } else {
        let scale = (glyph_ratio * 100.0).round().clamp(60.0, 160.0) as u16;
        (scale, pdf_width - word_w * scale as f32 / 100.0)
    };
    let spacing = (rest / n as f32).clamp(-0.2 * style.size, style.size);
    style.char_scale = scale;
    // Word stores spacing in twentieths of a point.
    style.char_spacing = (spacing * 20.0).round() / 20.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_parses() {
        let faces = builtin_faces();
        assert_eq!(faces.len(), 20);
        let lib = FontLibrary::new(false);
        let st = TextStyle { font: "Times New Roman".into(), size: 10.0, ..Default::default() };
        // Times "m" = 0.778 em, "c" = 0.444 em.
        let (w, unknown) = lib.measure("mc", &st).unwrap();
        assert_eq!(unknown, 0);
        assert!((w - 12.22).abs() < 0.05, "{w}");
        // Helvetica is measured as Arial; unknown families fall back by class.
        let st = TextStyle { font: "Helvetica".into(), size: 10.0, ..Default::default() };
        assert!(lib.measure("abc", &st).is_some());
        let st = TextStyle { font: "No Such Font".into(), generic: Generic::Mono, size: 10.0, ..Default::default() };
        let (w, _) = lib.measure("iiii", &st).unwrap();
        assert!((w - 24.0).abs() < 0.1, "monospace fallback {w}");
    }

    #[test]
    fn compensation_spacing_and_scale() {
        let lib = FontLibrary::new(false);
        let mut st = TextStyle { font: "Arial".into(), size: 10.0, ..Default::default() };
        let (w, _) = lib.measure("Hello", &st).unwrap();
        // 5% wider in the PDF → spacing.
        adjust("Hello", &mut st, w * 1.05, 0.0, &lib);
        assert_eq!(st.char_scale, 100);
        assert!((st.char_spacing - w * 0.05 / 5.0).abs() < 0.05);
        // 25% narrower (condensed font) → scaling.
        let mut st = TextStyle { font: "Arial".into(), size: 10.0, ..Default::default() };
        adjust("Hello", &mut st, w * 0.75, 0.0, &lib);
        assert_eq!(st.char_scale, 75);
        // Matching width → untouched.
        let mut st = TextStyle { font: "Arial".into(), size: 10.0, ..Default::default() };
        adjust("Hello", &mut st, w, 0.0, &lib);
        assert_eq!((st.char_scale, st.char_spacing), (100, 0.0));
        // 20% wider only because of letter spacing → spacing, not scaling.
        let mut st = TextStyle { font: "Arial".into(), size: 10.0, ..Default::default() };
        adjust("Hello", &mut st, w * 1.2, w * 0.2, &lib);
        assert_eq!(st.char_scale, 100);
        assert!((st.char_spacing - w * 0.2 / 5.0).abs() < 0.05);
    }
}
