//! Fonts for edited text: what the PDF's own fonts can show (learned from
//! their use and read from their embedded programs), installed fallback
//! fonts with shaping, and embedding them as subsets.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::anyhow;
use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat};

use super::content::Scan;
use super::objects::PObj;
use crate::metrics::{FontData, FontLibrary};

/// Identity of a font: its dictionary's object, or the owner and resource name.
pub(crate) type FontKey = (ObjectId, Vec<u8>);

pub(crate) struct FontMeta {
    pub key: FontKey,
    /// The font dictionary's object, when it is an indirect object.
    pub dict_id: Option<ObjectId>,
    /// Bytes per character code (1 for simple fonts, 2 for Identity-H CID fonts).
    pub code_len: Option<usize>,
    pub embedded: bool,
    pub simple: bool,
    pub subtype: Vec<u8>,
    /// Glyph widths in em by character code.
    pub widths: HashMap<u32, f64>,
    pub missing: f64,
    pub has_widths: bool,
    pub family: String,
    /// PostScript name without subset prefix.
    pub base: String,
    pub bold: bool,
    pub italic: bool,
    pub dict: Dictionary,
    /// TrueType CID font whose CIDs are glyph ids.
    pub cid_identity: bool,
    pub symbolic: bool,
    /// Embedded TrueType/OpenType program.
    pub program: Option<Arc<Vec<u8>>>,
}

pub(crate) fn deref<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a Object {
    doc.dereference(o).map(|(_, o)| o).unwrap_or(o)
}

pub(crate) fn obj_num(doc: &lopdf::Document, o: &Object) -> Option<f64> {
    match deref(doc, o) {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

/// A font name without its subset prefix ("ABCDEF+").
pub(crate) fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((p, rest)) if p.len() == 6 && p.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

/// The fonts of a resource dictionary.
pub(crate) fn resource_fonts(doc: &lopdf::Document, res: &Dictionary, owner: ObjectId) -> HashMap<Vec<u8>, FontMeta> {
    let mut out = HashMap::new();
    let Ok(fonts) = res.get(b"Font").map(|f| deref(doc, f)).and_then(|f| f.as_dict()) else { return out };
    for (name, v) in fonts.iter() {
        let (key, id) = match v {
            Object::Reference(id) => ((*id, Vec::new()), Some(*id)),
            _ => ((owner, name.clone()), None),
        };
        if let Ok(dict) = deref(doc, v).as_dict() {
            out.insert(name.clone(), font_meta(doc, key, id, dict));
        }
    }
    out
}

fn stream_bytes(doc: &lopdf::Document, o: &Object) -> Option<Vec<u8>> {
    let s = deref(doc, o).as_stream().ok()?;
    s.decompressed_content().ok().or_else(|| Some(s.content.clone()))
}

fn font_meta(doc: &lopdf::Document, key: FontKey, dict_id: Option<ObjectId>, dict: &Dictionary) -> FontMeta {
    let name_of = |d: &Dictionary, k: &[u8]| d.get(k).ok().map(|o| deref(doc, o)).and_then(|o| o.as_name().ok()).map(|n| n.to_vec());
    let subtype = name_of(dict, b"Subtype").unwrap_or_default();
    let base = name_of(dict, b"BaseFont").map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let info = crate::fonts::analyse_font_name(&base);
    let mut meta = FontMeta {
        key,
        dict_id,
        code_len: Some(1),
        embedded: false,
        simple: subtype != b"Type0",
        subtype: subtype.clone(),
        widths: HashMap::new(),
        missing: 0.0,
        has_widths: false,
        family: info.family,
        base: strip_subset(&base).to_string(),
        bold: info.bold,
        italic: info.italic,
        dict: dict.clone(),
        cid_identity: false,
        symbolic: false,
        program: None,
    };
    let descriptor = |d: &Dictionary| d.get(b"FontDescriptor").ok().and_then(|fd| deref(doc, fd).as_dict().ok().cloned());
    let program = |fd: &Dictionary| -> (bool, Option<Arc<Vec<u8>>>) {
        let has = fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3");
        let mut prog = None;
        if let Ok(f) = fd.get(b"FontFile2") {
            prog = stream_bytes(doc, f);
        } else if let Ok(f) = fd.get(b"FontFile3") {
            let open_type = deref(doc, f).as_stream().ok().and_then(|s| s.dict.get(b"Subtype").ok()).and_then(|o| o.as_name().ok()) == Some(b"OpenType");
            if open_type {
                prog = stream_bytes(doc, f);
            }
        }
        let prog = prog.filter(|p| ttf_parser::Face::parse(p, 0).is_ok()).map(Arc::new);
        (has, prog)
    };
    if subtype == b"Type0" {
        let enc = name_of(dict, b"Encoding");
        meta.code_len = match enc.as_deref() {
            Some(b"Identity-H") => Some(2),
            _ => None,
        };
        let desc = dict
            .get(b"DescendantFonts")
            .ok()
            .and_then(|a| deref(doc, a).as_array().ok())
            .and_then(|a| a.first())
            .and_then(|d| deref(doc, d).as_dict().ok());
        if let Some(desc) = desc {
            if let Some(fd) = descriptor(desc) {
                let (has, prog) = program(&fd);
                meta.embedded = has;
                meta.program = prog;
            }
            let cid_type2 = name_of(desc, b"Subtype").as_deref() == Some(b"CIDFontType2");
            let map_identity = match desc.get(b"CIDToGIDMap").map(|o| deref(doc, o)) {
                Ok(Object::Name(n)) => n == b"Identity",
                Ok(_) => false,
                Err(_) => true,
            };
            meta.cid_identity = cid_type2 && map_identity;
            meta.missing = desc.get(b"DW").ok().and_then(|o| obj_num(doc, o)).unwrap_or(1000.0) / 1000.0;
            meta.has_widths = true;
            if let Some(w) = desc.get(b"W").ok().and_then(|o| deref(doc, o).as_array().ok()) {
                let mut i = 0;
                while i < w.len() {
                    let Some(first) = obj_num(doc, &w[i]) else { break };
                    match w.get(i + 1).map(|o| deref(doc, o)) {
                        Some(Object::Array(ws)) => {
                            for (k, x) in ws.iter().enumerate() {
                                if let Some(v) = obj_num(doc, x) {
                                    meta.widths.insert(first as u32 + k as u32, v / 1000.0);
                                }
                            }
                            i += 2;
                        }
                        Some(_) => {
                            let (Some(last), Some(v)) = (obj_num(doc, &w[i + 1]), w.get(i + 2).and_then(|o| obj_num(doc, o))) else { break };
                            if last - first <= 65535.0 {
                                for c in first as u32..=last as u32 {
                                    meta.widths.insert(c, v / 1000.0);
                                }
                            }
                            i += 3;
                        }
                        None => break,
                    }
                }
            }
        }
    } else {
        let fd = descriptor(dict);
        if let Some(fd) = &fd {
            let (has, prog) = program(fd);
            meta.embedded = has || subtype == b"Type3";
            meta.program = prog;
            meta.symbolic = fd.get(b"Flags").ok().and_then(|o| obj_num(doc, o)).is_some_and(|f| (f as i64) & 4 != 0);
        } else {
            meta.embedded = subtype == b"Type3";
        }
        let scale = if subtype == b"Type3" {
            dict.get(b"FontMatrix").ok().and_then(|o| deref(doc, o).as_array().ok()).and_then(|a| a.first()).and_then(|o| obj_num(doc, o)).unwrap_or(0.001)
        } else {
            0.001
        };
        if let (Some(first), Some(ws)) = (
            dict.get(b"FirstChar").ok().and_then(|o| obj_num(doc, o)),
            dict.get(b"Widths").ok().and_then(|o| deref(doc, o).as_array().ok()),
        ) {
            meta.has_widths = true;
            for (k, x) in ws.iter().enumerate() {
                if let Some(v) = obj_num(doc, x) {
                    meta.widths.insert(first as u32 + k as u32, v * scale);
                }
            }
        }
        meta.missing = fd.as_ref().and_then(|fd| fd.get(b"MissingWidth").ok()).and_then(|o| obj_num(doc, o)).unwrap_or(0.0) * scale;
    }
    meta
}

pub(crate) fn split_codes(bytes: &[u8], len: usize) -> Vec<Vec<u8>> {
    bytes.chunks(len).filter(|c| c.len() == len).map(|c| c.to_vec()).collect()
}

pub(crate) fn code_value(code: &[u8]) -> u32 {
    code.iter().fold(0u32, |a, &b| (a << 8) | b as u32)
}

// ---------------------------------------------------------------------------
// Scripts
// ---------------------------------------------------------------------------

/// Right-to-left scripts (Hebrew, Arabic, …).
pub(crate) fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

/// Scripts whose letters change shape or order with their neighbours, so
/// that they must be shaped (Arabic, Indic, Thai, …), and combining marks.
pub(crate) fn needs_shaping(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F // combining marks
        | 0x0600..=0x08FF // Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic
        | 0x0900..=0x0DFF // Indic
        | 0x0E00..=0x0FFF // Thai, Lao, Tibetan
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF // Arabic presentation forms
    )
}

/// `text` in display order (right-to-left runs reversed).
pub(crate) fn visual_order(text: &str) -> String {
    if !text.chars().any(is_rtl) {
        return text.to_string();
    }
    let info = unicode_bidi::BidiInfo::new(text, None);
    let mut out = String::new();
    for para in &info.paragraphs {
        out.push_str(&info.reorder_line(para, para.range.clone()));
    }
    out
}

// ---------------------------------------------------------------------------
// Learning what a font can show
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct Learned {
    pub codes: HashMap<char, Vec<u8>>,
    pub gaps: Vec<f64>,
}

/// An installed font used for edited text.
pub(crate) struct Fallback {
    pub family: String,
    pub font: Arc<FontData>,
    /// Original glyph ids and the text they stand for.
    pub glyphs: BTreeMap<u16, String>,
    pub id: Option<ObjectId>,
    /// Original glyph id → glyph id in the embedded subset.
    pub map: HashMap<u16, u16>,
}

/// Glyphs added to a font of the PDF: (code, text, width in em).
pub(crate) type Additions = Vec<(Vec<u8>, String, f64)>;

pub(crate) struct Ctx {
    pub learned: HashMap<FontKey, Learned>,
    pub learned_pages: BTreeSet<usize>,
    pub lib: FontLibrary,
    pub notes: Vec<String>,
    pub fallbacks: Vec<Fallback>,
    pub additions: HashMap<ObjectId, Additions>,
}

impl Ctx {
    pub fn new() -> Self {
        Ctx {
            learned: HashMap::new(),
            learned_pages: BTreeSet::new(),
            lib: FontLibrary::new(true),
            notes: Vec::new(),
            fallbacks: Vec::new(),
            additions: HashMap::new(),
        }
    }

    pub fn note(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    pub fn note_substitution(&mut self, page: usize, original: &str, used: &str) {
        // The same family (e.g. a font embedded by an earlier edit) is no substitution.
        let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        if norm(original).starts_with(&norm(used)) || norm(used).starts_with(&norm(original)) {
            return;
        }
        self.note(format!("Page {}: “{original}” in this PDF lacks some of the new characters; they use {used} instead.", page + 1));
    }

    /// Index of the fallback entry for `font`.
    pub fn fallback(&mut self, family: &str, font: Arc<FontData>) -> usize {
        if let Some(i) = self.fallbacks.iter().position(|f| Arc::ptr_eq(&f.font, &font)) {
            return i;
        }
        self.fallbacks.push(Fallback { family: family.to_string(), font, glyphs: BTreeMap::new(), id: None, map: HashMap::new() });
        self.fallbacks.len() - 1
    }

    /// Records the codes a content stream used for the characters PDFium decoded.
    pub fn learn(&mut self, scan: &Scan, objs: &HashMap<usize, &PObj>, fonts: &HashMap<Vec<u8>, FontMeta>) {
        for sh in &scan.shows {
            let Some(meta) = fonts.get(&super::content::decode_name(&sh.font)) else { continue };
            let Some(len) = meta.code_len else { continue };
            let Some(o) = objs.get(&sh.object) else { continue };
            let entry = self.learned.entry(meta.key.clone()).or_default();
            let mut seen_str = false;
            for p in &sh.parts {
                match p {
                    super::content::Arg::Str(b) if !b.is_empty() => seen_str = true,
                    super::content::Arg::Num(n) if seen_str && *n <= -120.0 => entry.gaps.push(*n),
                    _ => {}
                }
            }
            if o.seq.iter().any(|(c, _)| needs_shaping(*c)) {
                continue;
            }
            let codes = split_codes(&sh.bytes(), len);
            let mut real: Vec<char> = o.seq.iter().filter(|(_, g)| !g).map(|(c, _)| *c).collect();
            if real.iter().any(|c| is_rtl(*c)) {
                // PDFium reports right-to-left text in reading order; the
                // codes are in drawing order. Only simple runs can be matched.
                if real.iter().any(|c| c.is_ascii_alphanumeric()) {
                    continue;
                }
                real.reverse();
            }
            if codes.len() != real.len() {
                continue;
            }
            for (c, code) in real.into_iter().zip(codes) {
                if c != '\u{FFFD}' {
                    entry.codes.entry(c).or_insert(code);
                }
            }
        }
    }
}

/// Code of `c` in a font that is not embedded (so it has every glyph its encoding names).
fn encode_simple(doc: &lopdf::Document, meta: &FontMeta, c: char) -> Option<Vec<u8>> {
    if !meta.simple || meta.subtype == b"Type3" {
        return None;
    }
    let enc = meta.dict.get_font_encoding(doc).ok()?;
    let s = c.to_string();
    let bytes = enc.string_to_bytes(&s);
    if bytes.len() != 1 || enc.bytes_to_string(&bytes).ok()? != s {
        return None;
    }
    Some(bytes)
}

/// A glyph of the font's embedded program for `c`: (code, width in em).
fn program_glyph(doc: &lopdf::Document, meta: &FontMeta, c: char) -> Option<(Vec<u8>, f64)> {
    let prog = meta.program.as_ref()?;
    let face = ttf_parser::Face::parse(prog, 0).ok()?;
    let gid = face.glyph_index(c)?;
    if gid.0 == 0 {
        return None;
    }
    // Subsets often keep unused glyph slots empty.
    struct Nop;
    impl ttf_parser::OutlineBuilder for Nop {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }
    if !c.is_whitespace() && face.outline_glyph(gid, &mut Nop).is_none() {
        return None;
    }
    let width = face.glyph_hor_advance(gid)? as f64 / face.units_per_em() as f64;
    if meta.code_len == Some(2) && meta.cid_identity {
        return Some((gid.0.to_be_bytes().to_vec(), width));
    }
    // A simple font's codes follow its /Encoding; without one they are the
    // font's own (subsets often number their glyphs 1, 2, 3 …).
    if meta.simple && meta.subtype == b"TrueType" && !meta.symbolic && meta.dict.has(b"Encoding") {
        let code = encode_simple(doc, meta, c)?;
        return Some((code, width));
    }
    None
}

// ---------------------------------------------------------------------------
// Pieces of new text
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) enum Piece {
    Glyph {
        /// Character code; for a fallback font the original glyph id (big endian).
        code: Vec<u8>,
        fb: Option<usize>,
        /// Width the font gives the glyph (em).
        em: f64,
        /// Advance after shaping (em).
        adv: f64,
        dx: f64,
        dy: f64,
        word_space: bool,
    },
    /// Space without a glyph, in em.
    Gap(f64),
}

/// Text space advance of `pieces` in a text state.
pub(crate) fn pieces_advance(pieces: &[Piece], size: f64, tc: f64, tw: f64, th: f64) -> f64 {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Glyph { adv, word_space, .. } => (adv * size + tc + if *word_space { tw } else { 0.0 }) * th,
            Piece::Gap(em) => em * size * th,
        })
        .sum()
}

/// Encodes `text` with a font of the PDF, if it is known to have every glyph.
/// Glyphs taken from the font's program are recorded as additions.
pub(crate) fn encode_in_font(ctx: &mut Ctx, doc: &lopdf::Document, meta: &FontMeta, text: &str) -> Option<Vec<Piece>> {
    if text.chars().any(needs_shaping) {
        return None;
    }
    let len = meta.code_len?;
    let text = visual_order(text);
    let learned = ctx.learned.get(&meta.key);
    let width = |code: &[u8], c: char, lib: &FontLibrary| -> f64 {
        if meta.has_widths {
            *meta.widths.get(&code_value(code)).unwrap_or(&meta.missing)
        } else {
            lib.advance(&meta.family, meta.bold, meta.italic, c).unwrap_or(0.5) as f64
        }
    };
    let gap = learned
        .map(|l| {
            let mut g = l.gaps.clone();
            g.sort_by(|a, b| a.total_cmp(b));
            g
        })
        .filter(|g| !g.is_empty())
        .map(|g| -g[g.len() / 2] / 1000.0)
        .unwrap_or(0.28);
    let mut out = Vec::new();
    let mut added: Additions = Vec::new();
    for c in text.chars() {
        let learned_code = learned.and_then(|l| l.codes.get(&c).cloned());
        let code = match learned_code {
            Some(code) => Some((code.clone(), width(&code, c, &ctx.lib))),
            None if !meta.embedded => encode_simple(doc, meta, c).map(|code| {
                let w = width(&code, c, &ctx.lib);
                (code, w)
            }),
            None => program_glyph(doc, meta, c).inspect(|(code, w)| {
                let known = meta.widths.contains_key(&code_value(code));
                if !known || meta.dict.has(b"ToUnicode") {
                    added.push((code.clone(), c.to_string(), *w));
                }
            }),
        };
        match code {
            Some((code, em)) => out.push(Piece::Glyph { word_space: len == 1 && code == [32], em, adv: em, dx: 0.0, dy: 0.0, code, fb: None }),
            None if c == ' ' => out.push(Piece::Gap(gap)),
            None => return None,
        }
    }
    if let Some(id) = meta.dict_id {
        if !added.is_empty() {
            let entry = ctx.additions.entry(id).or_default();
            for a in added {
                if !entry.iter().any(|e| e.0 == a.0) {
                    entry.push(a);
                }
            }
        }
    } else if !added.is_empty() {
        // A direct font dictionary cannot be extended in place.
        return None;
    }
    Some(out)
}

/// Width of a space in the font, in em, for layout.
pub(crate) fn space_em(ctx: &mut Ctx, doc: &lopdf::Document, meta: &FontMeta) -> f64 {
    match encode_in_font(ctx, doc, meta, " ") {
        Some(p) => pieces_advance(&p, 1.0, 0.0, 0.0, 1.0),
        None => 0.28,
    }
}

// ---------------------------------------------------------------------------
// Installed fonts
// ---------------------------------------------------------------------------

/// An installed font for text that the PDF's own font cannot show.
pub(crate) fn fallback_font(family: &str, base: &str, bold: bool, italic: bool, serif: bool, mono: bool, text: &str) -> Option<(String, Arc<FontData>)> {
    use crate::metrics::installed_font;
    let usable = |f: &Arc<FontData>| f.embeddable() && f.covers(text);
    let generic = match crate::fonts::generic_of(family) {
        crate::model::Generic::Mono => "mono",
        crate::model::Generic::Sans => "sans",
        crate::model::Generic::Serif => "serif",
        _ if mono => "mono",
        _ if serif => "serif",
        _ => "sans",
    };
    let lower = family.to_ascii_lowercase();
    let b = base.to_ascii_lowercase();
    let tex = (b.starts_with("cm") || b.starts_with("lm") || b.starts_with("sfrm") || b.contains("lmroman")) && !mono;
    let aliases: &[&str] = match lower.as_str() {
        _ if tex && b.contains("ss") => &["Latin Modern Sans", "CMU Sans Serif"],
        _ if tex => &["Latin Modern Roman", "LM Roman 10", "CMU Serif"],
        "times new roman" | "times" => &["Liberation Serif", "Tinos", "Nimbus Roman", "TeX Gyre Termes", "FreeSerif"],
        "arial" | "helvetica" => &["Liberation Sans", "Arimo", "Nimbus Sans", "TeX Gyre Heros", "FreeSans"],
        "courier new" | "courier" => &["Liberation Mono", "Cousine", "Nimbus Mono PS", "FreeMono"],
        "calibri" => &["Carlito"],
        "cambria" => &["Caladea"],
        _ => &[],
    };
    let generics: &[&str] = match generic {
        "mono" => &["Courier New", "Liberation Mono", "Cousine", "DejaVu Sans Mono", "Noto Sans Mono", "Consolas", "Menlo", "FreeMono"],
        "serif" => &["Times New Roman", "Liberation Serif", "Tinos", "DejaVu Serif", "Noto Serif", "Georgia", "FreeSerif"],
        _ => &["Arial", "Liberation Sans", "Arimo", "Helvetica", "DejaVu Sans", "Noto Sans", "Segoe UI", "Verdana", "FreeSans"],
    };
    let mut tried = BTreeSet::new();
    let first: &str = if tex { "" } else { family };
    for name in std::iter::once(first).chain(aliases.iter().copied()).chain(generics.iter().copied()) {
        if name.is_empty() || !tried.insert(name.to_ascii_lowercase()) {
            continue;
        }
        if let Some(f) = installed_font(name, bold, italic).filter(usable) {
            return Some((name.to_string(), f));
        }
    }
    // Anything installed that has the glyphs (CJK, Arabic, …), the
    // script's own fonts first.
    let mut families = crate::metrics::installed_families();
    let script_hint = script_family_hint(text);
    families.sort_by_key(|f| {
        let hinted = script_hint.iter().any(|h| f.contains(h));
        (!hinted, !(f.contains("noto") || f.contains("dejavu")), f.len())
    });
    for name in families {
        if !tried.insert(name.clone()) {
            continue;
        }
        if let Some(f) = installed_font(&name, bold, italic).filter(usable) {
            return Some((name, f));
        }
    }
    None
}

fn script_family_hint(text: &str) -> Vec<&'static str> {
    let mut v = Vec::new();
    for c in text.chars() {
        let h: &[&str] = match c as u32 {
            0x0590..=0x05FF => &["hebrew"],
            0x0600..=0x06FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => &["arabic", "naskh"],
            0x0900..=0x097F => &["devanagari"],
            0x0E00..=0x0E7F => &["thai"],
            0x3040..=0x30FF | 0x4E00..=0x9FFF => &["cjk", "gothic", "mincho"],
            0xAC00..=0xD7AF => &["cjk", "kr"],
            _ => &[],
        };
        for x in h {
            if !v.contains(x) {
                v.push(*x);
            }
        }
    }
    v
}

/// The installed face of `family` with exactly this style, if any.
pub(crate) fn installed_variant(family: &str, bold: bool, italic: bool, text: &str) -> Option<Arc<FontData>> {
    let f = crate::metrics::installed_font(family, bold, italic)?;
    let face = f.face()?;
    (face.is_bold() == bold && face.is_italic() == italic && f.embeddable() && f.covers(text)).then_some(f)
}

/// A shaped glyph (em units).
#[derive(Clone, Debug)]
pub(crate) struct Shaped {
    pub gid: u16,
    pub text: String,
    pub adv: f64,
    pub natural: f64,
    pub dx: f64,
    pub dy: f64,
}

/// Shapes `text` with `font` (kerning, ligatures, contextual forms,
/// right-to-left runs), glyphs in display order.
pub(crate) fn shape(font: &FontData, text: &str) -> Vec<Shaped> {
    let Some(face) = rustybuzz::Face::from_slice(font.bytes(), font.face_index()) else { return Vec::new() };
    let upem = face.units_per_em().max(1) as f64;
    let info = unicode_bidi::BidiInfo::new(text, None);
    let mut out = Vec::new();
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let sub = &text[run.clone()];
            if sub.is_empty() {
                continue;
            }
            let rtl = levels[run.start].is_rtl();
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(sub);
            buf.set_direction(if rtl { rustybuzz::Direction::RightToLeft } else { rustybuzz::Direction::LeftToRight });
            let glyphs = rustybuzz::shape(&face, &[], buf);
            let mut starts: Vec<usize> = glyphs.glyph_infos().iter().map(|g| g.cluster as usize).collect();
            starts.sort_unstable();
            starts.dedup();
            let mut given = BTreeSet::new();
            for (gi, gp) in glyphs.glyph_infos().iter().zip(glyphs.glyph_positions()) {
                let c = gi.cluster as usize;
                let end = starts.iter().find(|s| **s > c).copied().unwrap_or(sub.len());
                let text = if given.insert(c) { sub.get(c..end).unwrap_or("").to_string() } else { String::new() };
                let gid = gi.glyph_id as u16;
                let natural = face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap_or(0) as f64 / upem;
                out.push(Shaped { gid, text, adv: gp.x_advance as f64 / upem, natural, dx: gp.x_offset as f64 / upem, dy: gp.y_offset as f64 / upem });
            }
        }
    }
    out
}

/// Pieces of `text` in fallback font `fb` (registering the glyphs it uses).
pub(crate) fn fallback_pieces(ctx: &mut Ctx, fb: usize, text: &str) -> Vec<Piece> {
    let font = ctx.fallbacks[fb].font.clone();
    let shaped = shape(&font, text);
    let mut out = Vec::with_capacity(shaped.len());
    for g in shaped {
        let entry = ctx.fallbacks[fb].glyphs.entry(g.gid).or_default();
        if entry.is_empty() && !g.text.is_empty() {
            *entry = g.text.clone();
        }
        out.push(Piece::Glyph { code: g.gid.to_be_bytes().to_vec(), fb: Some(fb), em: g.natural, adv: g.adv, dx: g.dx, dy: g.dy, word_space: false });
    }
    out
}

/// Chooses an installed font for `text` and returns its pieces.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fallback_text(
    ctx: &mut Ctx,
    page: usize,
    note: Option<()>,
    family: &str,
    base: &str,
    bold: bool,
    italic: bool,
    serif: bool,
    mono: bool,
    text: &str,
) -> anyhow::Result<(usize, Vec<Piece>)> {
    let probe: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let (used, font) = fallback_font(family, base, bold, italic, serif, mono, &probe).ok_or_else(|| anyhow!("no installed font can show “{text}”"))?;
    if note.is_some() {
        ctx.note_substitution(page, if base.is_empty() { family } else { base }, &used);
    }
    let fb = ctx.fallback(&used, font);
    Ok((fb, fallback_pieces(ctx, fb, text)))
}

// ---------------------------------------------------------------------------
// Embedding installed fonts as subsets
// ---------------------------------------------------------------------------

fn ps_name(face: &ttf_parser::Face) -> String {
    face.names()
        .into_iter()
        .find(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
        .and_then(|n| n.to_string())
        .or_else(|| crate::metrics::face_families(face).into_iter().next())
        .unwrap_or_else(|| "Font".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

fn subset_tag(seed: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in seed.bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    (0..6).map(|i| (b'A' + ((h >> (i * 5)) % 26) as u8) as char).collect()
}

fn to_unicode_cmap(entries: &[(u16, String)]) -> Vec<u8> {
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (g, t) in chunk {
            let hex: String = t.encode_utf16().map(|u| format!("{u:04X}")).collect();
            cmap.push_str(&format!("<{g:04X}> <{hex}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    cmap.into_bytes()
}

/// Embeds the glyphs used from fallback font `fb` (a Type0/Identity-H
/// subset), extending a subset embedded by an earlier edit when there is one.
pub(crate) fn embed_fallback(doc: &mut lopdf::Document, fb: &mut Fallback) -> anyhow::Result<()> {
    let font = fb.font.clone();
    let face = font.face().ok_or_else(|| anyhow!("unreadable font {}", fb.family))?;
    let psname = ps_name(&face);
    let tag = format!("{psname}#{}", font.face_index());
    let existing = doc.objects.iter().find_map(|(id, o)| {
        let d = o.as_dict().ok()?;
        (d.get(b"P2WFont").ok()?.as_str().ok()? == tag.as_bytes()).then_some(*id)
    });
    // Glyph order of the existing subset: codes already in content streams stay valid.
    let mut order: Vec<u16> = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    if let Some(id) = existing {
        let d = doc.get_dictionary(id)?;
        if let Ok(g) = d.get(b"P2WGlyphs").and_then(|o| o.as_str()) {
            order = g.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        }
        if let Ok(t) = d.get(b"P2WText").and_then(|o| o.as_str()) {
            texts = String::from_utf8_lossy(t).split('\u{1f}').map(str::to_string).collect();
        }
        texts.resize(order.len(), String::new());
    }
    for (g, t) in &fb.glyphs {
        match order.iter().position(|o| o == g) {
            Some(i) => {
                if texts[i].is_empty() {
                    texts[i] = t.clone();
                }
            }
            None => {
                order.push(*g);
                texts.push(t.clone());
            }
        }
    }
    let mut remapper = subsetter::GlyphRemapper::new();
    for g in &order {
        remapper.remap(*g);
    }
    let data = subsetter::subset(font.bytes(), font.face_index(), &remapper).map_err(|e| anyhow!("could not subset {}: {e}", fb.family))?;
    fb.map = order.iter().filter_map(|g| remapper.get(*g).map(|n| (*g, n))).collect();
    let upem = face.units_per_em() as f64;
    let scale = |v: f64| (v * 1000.0 / upem).round() as i64;
    let mut w = Vec::new();
    let mut tu = Vec::new();
    for (i, g) in order.iter().enumerate() {
        let new = fb.map[g];
        let adv = face.glyph_hor_advance(ttf_parser::GlyphId(*g)).unwrap_or(0) as f64;
        w.push(Object::Integer(new as i64));
        w.push(Object::Array(vec![Object::Integer(scale(adv))]));
        if !texts[i].is_empty() {
            tu.push((new, texts[i].clone()));
        }
    }
    let cff = face.tables().cff.is_some();
    let file = if cff {
        let sub = ttf_parser::RawFace::parse(&data, 0).map_err(|_| anyhow!("bad subset"))?;
        let table = sub.table(ttf_parser::Tag::from_bytes(b"CFF ")).ok_or_else(|| anyhow!("subset without CFF table"))?.to_vec();
        let mut d = Dictionary::new();
        d.set("Subtype", Object::Name(b"CIDFontType0C".to_vec()));
        let mut s = Stream::new(d, table);
        let _ = s.compress();
        s
    } else {
        let mut d = Dictionary::new();
        d.set("Length1", data.len() as i64);
        let mut s = Stream::new(d, data);
        let _ = s.compress();
        s
    };
    let base = format!("{}+{psname}", subset_tag(&format!("{tag}{}", order.len())));
    let mut to_unicode = Stream::new(Dictionary::new(), to_unicode_cmap(&tu));
    let _ = to_unicode.compress();
    let glyph_bytes: Vec<u8> = order.iter().flat_map(|g| g.to_be_bytes()).collect();
    let text_joined = texts.join("\u{1f}");

    if let Some(id) = existing {
        let (desc_id, fd_id) = {
            let d = doc.get_dictionary(id)?;
            let desc_id = d.get(b"DescendantFonts").ok().and_then(|a| a.as_array().ok()).and_then(|a| a.first()).and_then(|o| o.as_reference().ok());
            let fd_id = desc_id.and_then(|i| doc.get_dictionary(i).ok()).and_then(|d| d.get(b"FontDescriptor").ok()).and_then(|o| o.as_reference().ok());
            (desc_id, fd_id)
        };
        let file_id = doc.add_object(file);
        let tu_id = doc.add_object(to_unicode);
        if let Some(fd) = fd_id {
            let fdd = doc.get_dictionary_mut(fd)?;
            fdd.set(if cff { "FontFile3" } else { "FontFile2" }, file_id);
            fdd.set("FontName", Object::Name(base.clone().into_bytes()));
        }
        if let Some(desc) = desc_id {
            let dd = doc.get_dictionary_mut(desc)?;
            dd.set("W", Object::Array(w));
            dd.set("BaseFont", Object::Name(base.clone().into_bytes()));
        }
        let d = doc.get_dictionary_mut(id)?;
        d.set("ToUnicode", tu_id);
        d.set("BaseFont", Object::Name(base.into_bytes()));
        d.set("P2WGlyphs", Object::String(glyph_bytes, StringFormat::Hexadecimal));
        d.set("P2WText", Object::String(text_joined.into_bytes(), StringFormat::Literal));
        fb.id = Some(id);
        return Ok(());
    }

    let file_id = doc.add_object(file);
    let bbox = face.global_bounding_box();
    let family = crate::metrics::face_families(&face).into_iter().next().unwrap_or_default();
    let mut flags = 4i64; // symbolic: the glyphs are addressed by id
    if face.is_monospaced() {
        flags |= 1;
    }
    if crate::fonts::generic_of(&family) == crate::model::Generic::Serif {
        flags |= 2;
    }
    if face.is_italic() {
        flags |= 64;
    }
    let mut fd = Dictionary::new();
    fd.set("Type", Object::Name(b"FontDescriptor".to_vec()));
    fd.set("FontName", Object::Name(base.clone().into_bytes()));
    fd.set("Flags", flags);
    fd.set(
        "FontBBox",
        Object::Array(vec![
            Object::Integer(scale(bbox.x_min as f64)),
            Object::Integer(scale(bbox.y_min as f64)),
            Object::Integer(scale(bbox.x_max as f64)),
            Object::Integer(scale(bbox.y_max as f64)),
        ]),
    );
    fd.set("ItalicAngle", Object::Real(face.italic_angle()));
    fd.set("Ascent", scale(face.ascender() as f64));
    fd.set("Descent", scale(face.descender() as f64));
    fd.set("CapHeight", scale(face.capital_height().unwrap_or(face.ascender()) as f64));
    fd.set("StemV", if face.is_bold() { 140i64 } else { 80i64 });
    fd.set(if cff { "FontFile3" } else { "FontFile2" }, file_id);
    let fd_id = doc.add_object(fd);
    let mut info = Dictionary::new();
    info.set("Registry", Object::string_literal("Adobe"));
    info.set("Ordering", Object::string_literal("Identity"));
    info.set("Supplement", 0i64);
    let mut cid = Dictionary::new();
    cid.set("Type", Object::Name(b"Font".to_vec()));
    cid.set("Subtype", Object::Name(if cff { b"CIDFontType0".to_vec() } else { b"CIDFontType2".to_vec() }));
    cid.set("BaseFont", Object::Name(base.clone().into_bytes()));
    cid.set("CIDSystemInfo", info);
    cid.set("FontDescriptor", fd_id);
    if !cff {
        cid.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    }
    cid.set("DW", 1000i64);
    cid.set("W", Object::Array(w));
    let cid_id = doc.add_object(cid);
    let tu_id = doc.add_object(to_unicode);
    let mut t0 = Dictionary::new();
    t0.set("Type", Object::Name(b"Font".to_vec()));
    t0.set("Subtype", Object::Name(b"Type0".to_vec()));
    t0.set("BaseFont", Object::Name(base.into_bytes()));
    t0.set("Encoding", Object::Name(b"Identity-H".to_vec()));
    t0.set("DescendantFonts", Object::Array(vec![Object::Reference(cid_id)]));
    t0.set("ToUnicode", tu_id);
    t0.set("P2WFont", Object::String(tag.into_bytes(), StringFormat::Literal));
    t0.set("P2WGlyphs", Object::String(glyph_bytes, StringFormat::Hexadecimal));
    t0.set("P2WText", Object::String(text_joined.into_bytes(), StringFormat::Literal));
    fb.id = Some(doc.add_object(t0));
    Ok(())
}

/// Adds widths and ToUnicode entries for glyphs of the PDF's own fonts that
/// edited text uses for the first time.
pub(crate) fn apply_additions(doc: &mut lopdf::Document, additions: &HashMap<ObjectId, Additions>) -> anyhow::Result<()> {
    for (id, adds) in additions {
        let dict = doc.get_dictionary(*id)?.clone();
        let type0 = dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()) == Some(b"Type0");
        if type0 {
            let desc = dict.get(b"DescendantFonts").ok().and_then(|a| deref(doc, a).as_array().ok()).and_then(|a| a.first()).cloned();
            if let Some(Object::Reference(desc_id)) = desc {
                let w_ref = doc.get_dictionary(desc_id)?.get(b"W").ok().cloned();
                let mut extra = Vec::new();
                for (code, _, w) in adds {
                    extra.push(Object::Integer(code_value(code) as i64));
                    extra.push(Object::Array(vec![Object::Real((*w * 1000.0) as f32)]));
                }
                match w_ref {
                    Some(Object::Reference(wid)) => {
                        if let Ok(Object::Array(a)) = doc.get_object_mut(wid) {
                            a.extend(extra);
                        }
                    }
                    Some(Object::Array(mut a)) => {
                        a.extend(extra);
                        doc.get_dictionary_mut(desc_id)?.set("W", Object::Array(a));
                    }
                    _ => doc.get_dictionary_mut(desc_id)?.set("W", Object::Array(extra)),
                }
            }
        } else {
            let first = dict.get(b"FirstChar").ok().and_then(|o| obj_num(doc, o)).unwrap_or(0.0) as i64;
            let widths_ref = dict.get(b"Widths").ok().cloned();
            let mut widths: Vec<Object> = match &widths_ref {
                Some(o) => deref(doc, o).as_array().cloned().unwrap_or_default(),
                None => Vec::new(),
            };
            let mut first_new = first;
            for (code, _, w) in adds {
                let c = code_value(code) as i64;
                if c < first_new {
                    let pad = (first_new - c) as usize;
                    let mut v = vec![Object::Integer(0); pad];
                    v.extend(widths);
                    widths = v;
                    first_new = c;
                }
                let idx = (c - first_new) as usize;
                if widths.len() <= idx {
                    widths.resize(idx + 1, Object::Integer(0));
                }
                widths[idx] = Object::Real((*w * 1000.0) as f32);
            }
            let last = first_new + widths.len() as i64 - 1;
            match widths_ref {
                Some(Object::Reference(wid)) if first_new == first => {
                    *doc.get_object_mut(wid)? = Object::Array(widths);
                }
                _ => doc.get_dictionary_mut(*id)?.set("Widths", Object::Array(widths)),
            }
            let d = doc.get_dictionary_mut(*id)?;
            d.set("FirstChar", first_new);
            d.set("LastChar", last);
        }
        // ToUnicode: append the new codes.
        if let Ok(Object::Reference(tu)) = dict.get(b"ToUnicode") {
            let tu = *tu;
            if let Ok(s) = doc.get_object(tu).and_then(|o| o.as_stream()) {
                let mut text = s.decompressed_content().unwrap_or_else(|_| s.content.clone());
                let mut block = format!("{} beginbfchar\n", adds.len());
                for (code, t, _) in adds {
                    let hex: String = code.iter().map(|b| format!("{b:02X}")).collect();
                    let dst: String = t.encode_utf16().map(|u| format!("{u:04X}")).collect();
                    block.push_str(&format!("<{hex}> <{dst}>\n"));
                }
                block.push_str("endbfchar\n");
                if let Some(pos) = find(&text, b"endcmap") {
                    text.splice(pos..pos, block.into_bytes());
                    let mut ns = Stream::new(Dictionary::new(), text);
                    let _ = ns.compress();
                    *doc.get_object_mut(tu)? = Object::Stream(ns);
                }
            }
        }
    }
    Ok(())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_order() {
        assert!(is_rtl('ש') && !is_rtl('a'));
        assert!(needs_shaping('ب') && !needs_shaping('ש'));
        assert_eq!(visual_order("abc"), "abc");
        assert_eq!(visual_order("שלום"), "םולש");
        assert_eq!(subset_tag("x").len(), 6);
        let cmap = String::from_utf8(to_unicode_cmap(&[(3, "fi".into())])).unwrap();
        assert!(cmap.contains("<0003> <00660069>"));
    }
}
