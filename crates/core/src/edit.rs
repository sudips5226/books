//! Editing the text of a PDF.
//!
//! PDFium finds the text runs of a page (their text, font, position and the
//! characters each font has already drawn). The edits themselves are made in
//! the page's content stream: only the operators that show the edited text
//! are rewritten and everything else stays byte for byte as it was. (PDFium
//! can regenerate a page's content, but it drops the kerning of every other
//! line on the page while doing so, which loses the word spaces of TeX and
//! many other PDFs.)
//!
//! Replacement text keeps the original font when that font is known to have
//! every needed glyph — PDFs usually embed only the glyphs they use, so this
//! is learned from the characters the font draws elsewhere in the document.
//! When a glyph is missing, the changed characters are set in a matching
//! installed font, which is embedded into the PDF.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use lopdf::{Dictionary, Object, ObjectId, Stream, StringFormat};
use pdfium_render::prelude::*;

use crate::extract::PageTransform;
use crate::metrics::{FontData, FontLibrary};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A run of text on a page that can be edited as a whole.
#[derive(Clone, Debug)]
pub struct TextItem {
    /// Indices of the page objects (as PDFium numbers them) that make up the run.
    pub objects: Vec<usize>,
    pub text: String,
    /// Bounding box as fractions of the displayed page (0..1, origin top-left).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Font family (subset prefix removed).
    pub font: String,
    /// Font size in points as it appears on the page.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    /// Invisible text, e.g. the text layer of an OCRed scan.
    pub invisible: bool,
}

/// Style of newly added text.
#[derive(Clone, Debug, Default)]
pub struct NewTextStyle {
    /// Font family; empty for a sans-serif default. "serif" and "mono" pick generic families.
    pub family: String,
    pub bold: bool,
    pub italic: bool,
}

/// One change to a PDF's text. Pages are 0-based; objects are [`TextItem::objects`].
#[derive(Clone, Debug)]
pub enum TextEdit {
    /// Replaces the text of a run. With `fit`, text that would be wider than the
    /// original is condensed to the original width.
    Replace { page: usize, objects: Vec<usize>, text: String, fit: bool },
    /// Removes a run (the space it took stays empty).
    Delete { page: usize, objects: Vec<usize> },
    /// Adds text whose top-left corner is at (`x`, `y`), fractions of the displayed page.
    Insert { page: usize, x: f32, y: f32, text: String, size: f32, color: [u8; 3], style: NewTextStyle },
}

impl TextEdit {
    fn page(&self) -> usize {
        match self {
            TextEdit::Replace { page, .. } | TextEdit::Delete { page, .. } | TextEdit::Insert { page, .. } => *page,
        }
    }
}

/// What happened while applying edits.
#[derive(Clone, Debug, Default)]
pub struct EditReport {
    /// Font substitutions and similar remarks, for the user.
    pub notes: Vec<String>,
}

/// The editable text runs of a page (0-based).
pub fn text_items(input: &Path, password: Option<&str>, page: usize, pdfium_path: Option<&Path>) -> anyhow::Result<Vec<TextItem>> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let p = document.pages().get(page as PdfPageIndex)?;
    let objs = page_objects(&p)?;
    let tf = PageTransform::new(&p);
    let (pw, ph) = (tf.width().max(1.0), tf.height().max(1.0));
    let mut items = Vec::new();
    for g in group_objects(&objs) {
        let text = group_text(&objs, &g);
        if text.trim().is_empty() {
            continue;
        }
        let first = &objs[g[0]];
        let mut r: Option<crate::model::Rect> = None;
        for &k in &g {
            let o = &objs[k];
            let rr = tf.rect(&PdfRect::new_from_values(o.bounds[1] as f32, o.bounds[0] as f32, o.bounds[3] as f32, o.bounds[2] as f32));
            r = Some(match r {
                Some(a) => a.union(&rr),
                None => rr,
            });
        }
        let r = r.unwrap_or_default();
        let info = crate::fonts::analyse_font_name(&first.font);
        let base = strip_subset(&first.font);
        items.push(TextItem {
            objects: g.iter().map(|&k| objs[k].index).collect(),
            text,
            x: r.x0 / pw,
            y: r.y0 / ph,
            w: r.width() / pw,
            h: r.height() / ph,
            font: if base.is_empty() { info.family } else { base.to_string() },
            size: first.size,
            bold: info.bold,
            italic: info.italic,
            color: first.color,
            invisible: first.invisible,
        });
    }
    Ok(items)
}

/// Applies `edits` to `input` and writes the result to `output` (which must
/// be a different file).
pub fn apply_edits(
    input: &Path,
    password: Option<&str>,
    edits: &[TextEdit],
    output: &Path,
    pdfium_path: Option<&Path>,
) -> anyhow::Result<EditReport> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let pdoc = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let mut doc = match password {
        Some(pw) if !pw.is_empty() => lopdf::Document::load_with_password(input, pw),
        _ => lopdf::Document::load(input),
    }
    .map_err(|e| anyhow!("this PDF cannot be edited ({e})"))?;
    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    if page_ids.len() != pdoc.pages().len() as usize {
        bail!("this PDF cannot be edited (its page tree is damaged)");
    }
    let mut ctx = Ctx { learned: HashMap::new(), learned_pages: BTreeSet::new(), lib: FontLibrary::new(true), report: EditReport::default() };
    let mut by_page: BTreeMap<usize, Vec<&TextEdit>> = BTreeMap::new();
    for e in edits {
        if e.page() >= page_ids.len() {
            bail!("page {} does not exist", e.page() + 1);
        }
        by_page.entry(e.page()).or_default().push(e);
    }
    for (page, list) in by_page {
        edit_page(&mut ctx, &pdoc, &mut doc, &page_ids, page, &list).with_context(|| format!("page {}", page + 1))?;
    }
    doc.prune_objects();
    doc.save(output).with_context(|| format!("could not write {}", output.display()))?;
    // The result must open again.
    drop(pdoc);
    pdfium
        .load_pdf_from_file(output, password)
        .map_err(|e| anyhow!("the edited PDF could not be read back ({e})"))?;
    Ok(ctx.report)
}

/// Replaces every occurrence of `find` in the document's text runs. Returns
/// the number of runs changed.
pub fn find_replace(
    input: &Path,
    password: Option<&str>,
    find: &str,
    replace: &str,
    output: &Path,
    pdfium_path: Option<&Path>,
) -> anyhow::Result<(usize, EditReport)> {
    if find.is_empty() {
        bail!("nothing to find");
    }
    let pages = crate::inspect(input, password, pdfium_path)?.pages;
    let mut edits = Vec::new();
    for page in 0..pages {
        for item in text_items(input, password, page, pdfium_path)? {
            if item.text.contains(find) {
                edits.push(TextEdit::Replace { page, objects: item.objects, text: item.text.replace(find, replace), fit: false });
            }
        }
    }
    let n = edits.len();
    if n == 0 {
        return Ok((0, EditReport::default()));
    }
    let report = apply_edits(input, password, &edits, output, pdfium_path)?;
    Ok((n, report))
}

/// Right-to-left scripts (Hebrew, Arabic, …): their characters are drawn in
/// visual order and Arabic needs contextual shaping, so they are not edited.
fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

const RTL_UNSUPPORTED: &str = "editing right-to-left text (Hebrew, Arabic …) is not supported yet";

/// A font name without its subset prefix ("ABCDEF+").
fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((p, rest)) if p.len() == 6 && p.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

// ---------------------------------------------------------------------------
// Text objects as PDFium sees them
// ---------------------------------------------------------------------------

struct PObj {
    /// Index among all objects of the page.
    index: usize,
    /// Origin of the first glyph (page space).
    origin: (f64, f64),
    /// Linear part of the text rendering matrix (page space).
    m: [f64; 4],
    /// Characters in order: (char, generated by PDFium rather than drawn).
    seq: Vec<(char, bool)>,
    font: String,
    serif: bool,
    mono: bool,
    size: f32,
    color: [u8; 3],
    invisible: bool,
    /// Bounds: left, bottom, right, top (page space).
    bounds: [f64; 4],
}

impl PObj {
    fn dir(&self) -> (f64, f64) {
        let l = (self.m[0] * self.m[0] + self.m[1] * self.m[1]).sqrt().max(1e-9);
        (self.m[0] / l, self.m[1] / l)
    }

    fn text(&self) -> String {
        self.seq.iter().map(|(c, _)| *c).collect()
    }

    fn corners(&self) -> [(f64, f64); 4] {
        let [l, b, r, t] = self.bounds;
        [(l, b), (r, b), (r, t), (l, t)]
    }
}

fn page_objects(page: &PdfPage) -> anyhow::Result<Vec<PObj>> {
    let tp = page.text()?;
    let mut out = Vec::new();
    for (index, object) in page.objects().iter().enumerate() {
        let Some(t) = object.as_text_object() else { continue };
        let Ok(m) = t.matrix() else { continue };
        let mut seq = Vec::new();
        if let Ok(chars) = tp.chars_for_object(t) {
            for ch in chars.iter() {
                let generated = ch.is_generated().unwrap_or(false);
                let c = ch.unicode_char().unwrap_or('\u{FFFD}');
                if generated && (c == '\r' || c == '\n') {
                    continue;
                }
                // PDFium reports TeX's hyphenation hyphen as U+0002.
                let c = if c == '\u{2}' { '-' } else { c };
                seq.push((c, generated));
            }
        }
        let font = t.font();
        let b = object.bounds().map(|q| q.to_rect()).ok();
        let bounds = b.map(|r| [r.left().value as f64, r.bottom().value as f64, r.right().value as f64, r.top().value as f64]).unwrap_or([
            m.e() as f64,
            m.f() as f64,
            m.e() as f64,
            m.f() as f64,
        ]);
        let color = object.fill_color().map(|c| [c.red(), c.green(), c.blue()]).unwrap_or([0, 0, 0]);
        out.push(PObj {
            index,
            origin: (m.e() as f64, m.f() as f64),
            m: [m.a() as f64, m.b() as f64, m.c() as f64, m.d() as f64],
            seq,
            font: font.name(),
            serif: font.is_serif(),
            mono: font.is_fixed_pitch(),
            size: t.scaled_font_size().value,
            color,
            invisible: matches!(t.render_mode(), PdfPageTextRenderMode::Invisible),
            bounds,
        });
    }
    Ok(out)
}

/// Groups consecutive text objects that continue the same line in the same style.
fn group_objects(objs: &[PObj]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut end = 0.0f64;
    for (k, o) in objs.iter().enumerate() {
        if let Some(g) = groups.last_mut() {
            let first = &objs[g[0]];
            let size = first.size.max(1.0) as f64;
            let (u, v) = (first.dir(), o.dir());
            let rel = (o.origin.0 - first.origin.0, o.origin.1 - first.origin.1);
            let along = rel.0 * u.0 + rel.1 * u.1;
            let across = -rel.0 * u.1 + rel.1 * u.0;
            let gap = along - end;
            if o.font == first.font
                && (o.size - first.size).abs() <= 0.05 * first.size + 0.01
                && o.color == first.color
                && o.invisible == first.invisible
                && u.0 * v.0 + u.1 * v.1 > 0.999
                && across.abs() <= 0.2 * size
                && gap >= -0.5 * size
                && gap <= 1.0 * size
            {
                g.push(k);
                end = end.max(extent(first, o));
                continue;
            }
        }
        groups.push(vec![k]);
        end = extent(o, o);
    }
    groups
}

/// How far `o` reaches along the baseline of `first`, measured from `first`'s origin.
fn extent(first: &PObj, o: &PObj) -> f64 {
    let u = first.dir();
    o.corners().iter().map(|c| (c.0 - first.origin.0) * u.0 + (c.1 - first.origin.1) * u.1).fold(0.0, f64::max)
}

fn group_text(objs: &[PObj], g: &[usize]) -> String {
    let mut text = String::new();
    let first = &objs[g[0]];
    let size = first.size.max(1.0) as f64;
    let mut end = 0.0f64;
    for (i, &k) in g.iter().enumerate() {
        let o = &objs[k];
        let t = o.text();
        if i > 0 {
            let u = first.dir();
            let along = (o.origin.0 - first.origin.0) * u.0 + (o.origin.1 - first.origin.1) * u.1;
            if along - end > 0.18 * size && !text.ends_with(char::is_whitespace) && !t.starts_with(char::is_whitespace) {
                text.push(' ');
            }
        }
        text.push_str(&t);
        end = end.max(extent(first, o));
    }
    text
}

// ---------------------------------------------------------------------------
// Content stream lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Arg {
    Num(f64),
    Str(Vec<u8>),
    /// A name, as written (still `#xx`-escaped).
    Name(Vec<u8>),
    Array(Vec<Arg>),
    Other,
}

#[derive(Clone, Debug)]
struct Op {
    /// Byte range of the operator together with its operands.
    start: usize,
    end: usize,
    name: Vec<u8>,
    args: Vec<Arg>,
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lexer<'a> {
    d: &'a [u8],
    p: usize,
}

impl Lexer<'_> {
    fn skip_ws(&mut self) {
        loop {
            while self.p < self.d.len() && is_ws(self.d[self.p]) {
                self.p += 1;
            }
            if self.p < self.d.len() && self.d[self.p] == b'%' {
                while self.p < self.d.len() && self.d[self.p] != b'\n' && self.d[self.p] != b'\r' {
                    self.p += 1;
                }
                continue;
            }
            break;
        }
    }

    fn regular(&mut self) -> &[u8] {
        let s = self.p;
        while self.p < self.d.len() && !is_ws(self.d[self.p]) && !is_delim(self.d[self.p]) {
            self.p += 1;
        }
        &self.d[s..self.p]
    }

    fn literal(&mut self) -> Vec<u8> {
        let d = self.d;
        self.p += 1;
        let mut depth = 1;
        let mut out = Vec::new();
        while self.p < d.len() {
            let c = d[self.p];
            self.p += 1;
            match c {
                b'\\' => {
                    let Some(&e) = d.get(self.p) else { break };
                    self.p += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match d.get(self.p) {
                                    Some(&o @ b'0'..=b'7') => {
                                        v = v * 8 + (o - b'0') as u32;
                                        self.p += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        b'\r' => {
                            if d.get(self.p) == Some(&b'\n') {
                                self.p += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\r' => {
                    if d.get(self.p) == Some(&b'\n') {
                        self.p += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(c),
            }
        }
        out
    }

    fn hex(&mut self) -> Vec<u8> {
        self.p += 1;
        let mut digits = Vec::new();
        while self.p < self.d.len() && self.d[self.p] != b'>' {
            let c = self.d[self.p];
            if let Some(v) = (c as char).to_digit(16) {
                digits.push(v as u8);
            }
            self.p += 1;
        }
        self.p += 1;
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        digits.chunks(2).map(|c| c[0] * 16 + c[1]).collect()
    }

    fn object(&mut self, depth: usize) -> Option<Arg> {
        self.skip_ws();
        let d = self.d;
        let &b = d.get(self.p)?;
        if depth > 64 {
            self.p += 1;
            return Some(Arg::Other);
        }
        Some(match b {
            b'(' => Arg::Str(self.literal()),
            b'<' if d.get(self.p + 1) == Some(&b'<') => {
                self.p += 2;
                loop {
                    self.skip_ws();
                    if self.p >= d.len() {
                        break;
                    }
                    if d[self.p] == b'>' {
                        self.p += if d.get(self.p + 1) == Some(&b'>') { 2 } else { 1 };
                        break;
                    }
                    if self.object(depth + 1).is_none() {
                        break;
                    }
                }
                Arg::Other
            }
            b'<' => Arg::Str(self.hex()),
            b'[' => {
                self.p += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.p >= d.len() {
                        break;
                    }
                    if d[self.p] == b']' {
                        self.p += 1;
                        break;
                    }
                    match self.object(depth + 1) {
                        Some(a) => items.push(a),
                        None => break,
                    }
                }
                Arg::Array(items)
            }
            b'/' => {
                self.p += 1;
                Arg::Name(self.regular().to_vec())
            }
            b'{' | b'}' | b')' | b'>' | b']' => {
                self.p += 1;
                Arg::Other
            }
            _ => {
                let t = self.regular();
                if t.is_empty() {
                    self.p += 1;
                    return Some(Arg::Other);
                }
                if t[0].is_ascii_digit() || matches!(t[0], b'+' | b'-' | b'.') {
                    Arg::Num(parse_number(t))
                } else {
                    Arg::Other
                }
            }
        })
    }
}

fn parse_number(t: &[u8]) -> f64 {
    let s = String::from_utf8_lossy(t);
    if let Ok(v) = s.parse::<f64>() {
        return v;
    }
    // Lenient: "--1", "1.2.3", "-.5-" …
    let neg = s.starts_with('-');
    let body: String = s.trim_start_matches(['+', '-']).chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut parts = body.splitn(3, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    let v = format!("{}.{}", if int.is_empty() { "0" } else { int }, if frac.is_empty() { "0" } else { frac }).parse::<f64>().unwrap_or(0.0);
    if neg {
        -v
    } else {
        v
    }
}

/// Position just after the `EI` of an inline image whose `BI` ends at `from`.
fn skip_inline_image(d: &[u8], from: usize) -> anyhow::Result<usize> {
    let mut i = from;
    loop {
        if i + 2 > d.len() {
            bail!("inline image without data");
        }
        if &d[i..i + 2] == b"ID" && (is_ws(d[i - 1]) || is_delim(d[i - 1])) && (i + 2 == d.len() || is_ws(d[i + 2])) {
            i += 3;
            break;
        }
        i += 1;
    }
    while i + 2 <= d.len() {
        if &d[i..i + 2] == b"EI" && is_ws(d[i - 1]) && (i + 2 == d.len() || is_ws(d[i + 2]) || is_delim(d[i + 2])) {
            return Ok(i + 2);
        }
        i += 1;
    }
    bail!("unterminated inline image")
}

fn parse_content(d: &[u8]) -> anyhow::Result<Vec<Op>> {
    let mut lx = Lexer { d, p: 0 };
    let mut ops = Vec::new();
    let mut args = Vec::new();
    let mut start: Option<usize> = None;
    loop {
        lx.skip_ws();
        if lx.p >= d.len() {
            break;
        }
        let b = d[lx.p];
        if matches!(b, b'(' | b'<' | b'[' | b'/' | b'{' | b'}' | b')' | b'>' | b']') || b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.') {
            start.get_or_insert(lx.p);
            if let Some(a) = lx.object(0) {
                args.push(a);
            }
            continue;
        }
        let s = lx.p;
        let word = lx.regular().to_vec();
        if word.is_empty() {
            lx.p += 1;
            continue;
        }
        if matches!(word.as_slice(), b"true" | b"false" | b"null") {
            start.get_or_insert(s);
            args.push(Arg::Other);
            continue;
        }
        let op_start = start.take().unwrap_or(s);
        if word == b"BI" {
            let end = skip_inline_image(d, lx.p)?;
            lx.p = end;
            args.clear();
            ops.push(Op { start: op_start, end, name: word, args: Vec::new() });
            continue;
        }
        ops.push(Op { start: op_start, end: lx.p, name: word, args: std::mem::take(&mut args) });
    }
    Ok(ops)
}

/// Decodes `#xx` escapes of a name as written in a content stream.
fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#' && i + 2 < raw.len() {
            if let Ok(v) = u8::from_str_radix(&String::from_utf8_lossy(&raw[i + 1..i + 3]), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Following the text position through a page's content
// ---------------------------------------------------------------------------

type Mat = [f64; 6];
const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m` followed by `n`.
fn mul(m: &Mat, n: &Mat) -> Mat {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

fn apply(m: &Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn translate(x: f64, y: f64) -> Mat {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

#[derive(Clone)]
struct TextState {
    /// Font resource name as written.
    font: Vec<u8>,
    size: f64,
    tc: f64,
    tw: f64,
    /// Horizontal scaling as a fraction.
    th: f64,
    tl: f64,
    rise: f64,
}

#[derive(Clone)]
struct GState {
    ctm: Mat,
    ts: TextState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ShowKind {
    Tj,
    TJ,
    Quote,
    DQuote,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Code(Vec<u8>),
    /// A TJ adjustment in thousandths of a text space unit.
    Kern(f64),
}

/// A text-showing operator that draws glyphs.
struct Show {
    op: usize,
    kind: ShowKind,
    /// Index of the matching PDFium page object.
    object: usize,
    font: Vec<u8>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tlm: Mat,
    /// Offset of the first glyph from the line start (text space).
    s: f64,
    /// Strings and adjustments in order (strings not yet split into codes).
    parts: Vec<Arg>,
    /// Advance from the first glyph to the end of the operator (text space),
    /// when a following operator continues from there.
    advance: Option<f64>,
}

impl Show {
    fn unit(&self) -> f64 {
        self.size * self.th / 1000.0
    }

    fn tm(&self) -> Mat {
        mul(&translate(self.s, 0.0), &self.tlm)
    }

    fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for p in &self.parts {
            if let Arg::Str(b) = p {
                v.extend_from_slice(b);
            }
        }
        v
    }
}

struct PageScan {
    ops: Vec<Op>,
    shows: Vec<Show>,
    /// Lowest q/Q nesting reached (negative with stray Q operators) and the final nesting.
    min_depth: i64,
    end_depth: i64,
}

/// Follows the text position through `content` and pairs every glyph-showing
/// operator with PDFium's text object (`objs`: object index and origin, in order).
fn scan_page(content: &[u8], objs: &[(usize, (f64, f64))]) -> anyhow::Result<PageScan> {
    let ops = parse_content(content)?;
    let mut st = GState { ctm: IDENTITY, ts: TextState { font: Vec::new(), size: 0.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0 } };
    let mut stack: Vec<GState> = Vec::new();
    let (mut depth, mut min_depth) = (0i64, 0i64);
    let mut tlm = IDENTITY;
    let mut s = 0.0f64;
    let mut known = true;
    let mut pending: Option<usize> = None;
    let mut extra = 0.0f64;
    let mut cursor = 0usize;
    let mut shows: Vec<Show> = Vec::new();
    let tol = |p: (f64, f64)| 0.6 + 1e-4 * (p.0.abs() + p.1.abs());

    for (i, op) in ops.iter().enumerate() {
        let a = &op.args;
        let num = |k: usize| -> f64 {
            match a.get(k) {
                Some(Arg::Num(v)) => *v,
                _ => 0.0,
            }
        };
        let line_move = |tlm: &mut Mat, s: &mut f64, x: f64, y: f64| {
            *tlm = mul(&translate(x, y), tlm);
            *s = 0.0;
        };
        match op.name.as_slice() {
            b"q" => {
                stack.push(st.clone());
                depth += 1;
            }
            b"Q" => {
                if let Some(g) = stack.pop() {
                    st = g;
                }
                depth -= 1;
                min_depth = min_depth.min(depth);
            }
            b"cm" if a.len() >= 6 => st.ctm = mul(&[num(0), num(1), num(2), num(3), num(4), num(5)], &st.ctm),
            b"BT" => {
                tlm = IDENTITY;
                s = 0.0;
                known = true;
                pending = None;
            }
            b"ET" => {
                known = true;
                pending = None;
            }
            b"Tm" if a.len() >= 6 => {
                tlm = [num(0), num(1), num(2), num(3), num(4), num(5)];
                s = 0.0;
                known = true;
                pending = None;
            }
            b"Td" => {
                line_move(&mut tlm, &mut s, num(0), num(1));
                known = true;
                pending = None;
            }
            b"TD" => {
                st.ts.tl = -num(1);
                line_move(&mut tlm, &mut s, num(0), num(1));
                known = true;
                pending = None;
            }
            b"T*" => {
                line_move(&mut tlm, &mut s, 0.0, -st.ts.tl);
                known = true;
                pending = None;
            }
            b"Tc" => st.ts.tc = num(0),
            b"Tw" => st.ts.tw = num(0),
            b"Tz" => st.ts.th = num(0) / 100.0,
            b"TL" => st.ts.tl = num(0),
            b"Ts" => st.ts.rise = num(0),
            b"Tf" => {
                if let Some(Arg::Name(n)) = a.first() {
                    st.ts.font = n.clone();
                }
                st.ts.size = num(1);
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                let kind = match op.name.as_slice() {
                    b"Tj" => ShowKind::Tj,
                    b"TJ" => ShowKind::TJ,
                    b"'" => ShowKind::Quote,
                    _ => ShowKind::DQuote,
                };
                if kind == ShowKind::DQuote {
                    st.ts.tw = num(0);
                    st.ts.tc = num(1);
                }
                if matches!(kind, ShowKind::Quote | ShowKind::DQuote) {
                    line_move(&mut tlm, &mut s, 0.0, -st.ts.tl);
                    known = true;
                    pending = None;
                }
                let parts: Vec<Arg> = match kind {
                    ShowKind::TJ => match a.first() {
                        Some(Arg::Array(v)) => v.iter().filter(|x| matches!(x, Arg::Num(_) | Arg::Str(_))).cloned().collect(),
                        _ => Vec::new(),
                    },
                    _ => a.iter().rev().find(|x| matches!(x, Arg::Str(_))).cloned().into_iter().collect(),
                };
                let glyphs = parts.iter().any(|p| matches!(p, Arg::Str(b) if !b.is_empty()));
                let unit = st.ts.size * st.ts.th / 1000.0;
                // Adjustments before the first glyph.
                let mut init = 0.0;
                for p in &parts {
                    match p {
                        Arg::Num(n) => init += n,
                        Arg::Str(b) if !b.is_empty() => break,
                        _ => {}
                    }
                }
                if !glyphs {
                    let dk = -init * unit;
                    if known {
                        s += dk;
                    } else {
                        extra += dk;
                    }
                    continue;
                }
                let init_disp = -init * unit;
                let Some(&(object, origin)) = objs.get(cursor) else {
                    bail!("the page content shows more text than PDFium reports");
                };
                cursor += 1;
                if known {
                    s += init_disp;
                    let m = mul(&mul(&translate(s, 0.0), &tlm), &st.ctm);
                    let o = apply(&m, 0.0, st.ts.rise);
                    if (o.0 - origin.0).hypot(o.1 - origin.1) > tol(o) {
                        bail!("could not locate text object {object} in the page content");
                    }
                } else {
                    // Continues where the previous operator ended: PDFium knows where that is.
                    let m = mul(&mul(&translate(s, 0.0), &tlm), &st.ctm);
                    let base = apply(&m, 0.0, st.ts.rise);
                    let uu = m[0] * m[0] + m[1] * m[1];
                    if uu < 1e-12 {
                        bail!("degenerate text matrix");
                    }
                    let dx = ((origin.0 - base.0) * m[0] + (origin.1 - base.1) * m[1]) / uu;
                    let o = apply(&m, dx, st.ts.rise);
                    if (o.0 - origin.0).hypot(o.1 - origin.1) > tol(o) {
                        bail!("could not follow the text position to object {object}");
                    }
                    if let Some(p) = pending {
                        shows[p].advance = Some(dx - extra - init_disp);
                    }
                    s += dx;
                }
                shows.push(Show {
                    op: i,
                    kind,
                    object,
                    font: st.ts.font.clone(),
                    size: st.ts.size,
                    tc: st.ts.tc,
                    tw: st.ts.tw,
                    th: st.ts.th,
                    tlm,
                    s,
                    parts,
                    advance: None,
                });
                pending = Some(shows.len() - 1);
                extra = 0.0;
                known = false;
            }
            _ => {}
        }
    }
    if cursor != objs.len() {
        bail!("PDFium reports text that is not in the page content (e.g. inside a form XObject)");
    }
    Ok(PageScan { ops, shows, min_depth, end_depth: depth })
}

// ---------------------------------------------------------------------------
// Fonts of a page
// ---------------------------------------------------------------------------

/// Identity of a font: its dictionary's object, or the page and resource name.
type FontKey = (ObjectId, Vec<u8>);

struct FontMeta {
    key: FontKey,
    /// Bytes per character code (1 for simple fonts, 2 for Identity-H CID fonts).
    code_len: Option<usize>,
    embedded: bool,
    simple: bool,
    /// Glyph widths in em by character code.
    widths: HashMap<u32, f64>,
    missing: f64,
    has_widths: bool,
    family: String,
    /// PostScript name without subset prefix.
    base: String,
    bold: bool,
    italic: bool,
    dict: Dictionary,
}

fn deref<'a>(doc: &'a lopdf::Document, o: &'a Object) -> &'a Object {
    doc.dereference(o).map(|(_, o)| o).unwrap_or(o)
}

fn obj_num(doc: &lopdf::Document, o: &Object) -> Option<f64> {
    match deref(doc, o) {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(*r as f64),
        _ => None,
    }
}

/// The page's resource dictionary (inherited ones included).
fn page_resources(doc: &lopdf::Document, page_id: ObjectId) -> Option<Dictionary> {
    let mut id = page_id;
    for _ in 0..64 {
        let d = doc.get_dictionary(id).ok()?;
        if let Ok(r) = d.get(b"Resources") {
            return deref(doc, r).as_dict().ok().cloned();
        }
        id = d.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn page_fonts(doc: &lopdf::Document, page_id: ObjectId) -> HashMap<Vec<u8>, FontMeta> {
    let mut out = HashMap::new();
    let Some(res) = page_resources(doc, page_id) else { return out };
    let Ok(fonts) = res.get(b"Font").map(|f| deref(doc, f)).and_then(|f| f.as_dict()) else { return out };
    for (name, v) in fonts.iter() {
        let key = match v {
            Object::Reference(id) => (*id, Vec::new()),
            _ => (page_id, name.clone()),
        };
        if let Ok(dict) = deref(doc, v).as_dict() {
            out.insert(name.clone(), font_meta(doc, key, dict));
        }
    }
    out
}

fn font_meta(doc: &lopdf::Document, key: FontKey, dict: &Dictionary) -> FontMeta {
    let name_of = |d: &Dictionary, k: &[u8]| d.get(k).ok().map(|o| deref(doc, o)).and_then(|o| o.as_name().ok()).map(|n| n.to_vec());
    let subtype = name_of(dict, b"Subtype").unwrap_or_default();
    let base = name_of(dict, b"BaseFont").map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let info = crate::fonts::analyse_font_name(&base);
    let mut meta = FontMeta {
        key,
        code_len: Some(1),
        embedded: false,
        simple: subtype != b"Type0",
        widths: HashMap::new(),
        missing: 0.0,
        has_widths: false,
        family: info.family,
        base: strip_subset(&base).to_string(),
        bold: info.bold,
        italic: info.italic,
        dict: dict.clone(),
    };
    let has_file = |d: &Dictionary| {
        d.get(b"FontDescriptor")
            .ok()
            .and_then(|fd| deref(doc, fd).as_dict().ok())
            .is_some_and(|fd| fd.has(b"FontFile") || fd.has(b"FontFile2") || fd.has(b"FontFile3"))
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
            meta.embedded = has_file(desc);
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
        meta.embedded = subtype == b"Type3" || has_file(dict);
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
        meta.missing = dict
            .get(b"FontDescriptor")
            .ok()
            .and_then(|fd| deref(doc, fd).as_dict().ok())
            .and_then(|fd| fd.get(b"MissingWidth").ok())
            .and_then(|o| obj_num(doc, o))
            .unwrap_or(0.0)
            * scale;
    }
    meta
}

fn split_codes(bytes: &[u8], len: usize) -> Vec<Vec<u8>> {
    bytes.chunks(len).filter(|c| c.len() == len).map(|c| c.to_vec()).collect()
}

fn code_value(code: &[u8]) -> u32 {
    code.iter().fold(0u32, |a, &b| (a << 8) | b as u32)
}

/// What a font is known to be able to draw: characters it has drawn and the
/// codes it used for them, and the word gaps of TJ arrays.
#[derive(Default)]
struct Learned {
    codes: HashMap<char, Vec<u8>>,
    gaps: Vec<f64>,
}

struct Ctx {
    learned: HashMap<FontKey, Learned>,
    learned_pages: BTreeSet<usize>,
    lib: FontLibrary,
    report: EditReport,
}

/// Records the codes `scan`'s operators used for the characters PDFium decoded.
fn learn(ctx: &mut Ctx, scan: &PageScan, objs: &HashMap<usize, &PObj>, fonts: &HashMap<Vec<u8>, FontMeta>) {
    for sh in &scan.shows {
        let Some(meta) = fonts.get(&decode_name(&sh.font)) else { continue };
        let Some(len) = meta.code_len else { continue };
        let Some(o) = objs.get(&sh.object) else { continue };
        let entry = ctx.learned.entry(meta.key.clone()).or_default();
        let mut seen_str = false;
        for p in &sh.parts {
            match p {
                Arg::Str(b) if !b.is_empty() => seen_str = true,
                Arg::Num(n) if seen_str && *n <= -120.0 => entry.gaps.push(*n),
                _ => {}
            }
        }
        if o.seq.iter().any(|(c, _)| is_rtl(*c)) {
            continue;
        }
        let codes = split_codes(&sh.bytes(), len);
        let real: Vec<char> = o.seq.iter().filter(|(_, g)| !g).map(|(c, _)| *c).collect();
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

/// Learns from the other pages of the document until `font` is known to have all of `chars`.
fn learn_more(ctx: &mut Ctx, pdoc: &PdfDocument, doc: &lopdf::Document, page_ids: &[ObjectId], font: &FontKey, chars: &BTreeSet<char>) {
    let missing = |ctx: &Ctx| chars.iter().any(|c| !ctx.learned.get(font).is_some_and(|l| l.codes.contains_key(c)));
    for page in 0..page_ids.len() {
        if !missing(ctx) {
            break;
        }
        if !ctx.learned_pages.insert(page) {
            continue;
        }
        let Ok(p) = pdoc.pages().get(page as PdfPageIndex) else { continue };
        let Ok(objs) = page_objects(&p) else { continue };
        let fonts = page_fonts(doc, page_ids[page]);
        if !fonts.values().any(|m| &m.key == font) {
            continue;
        }
        let content = doc.get_page_content(page_ids[page]);
        let pairs: Vec<(usize, (f64, f64))> = objs.iter().map(|o| (o.index, o.origin)).collect();
        if let Ok(scan) = scan_page(&content, &pairs) {
            let by_index: HashMap<usize, &PObj> = objs.iter().map(|o| (o.index, o)).collect();
            learn(ctx, &scan, &by_index, &fonts);
        }
    }
}

/// Code of `c` in a font that is not embedded (so it has every glyph its encoding names).
fn encode_unembedded(doc: &lopdf::Document, meta: &FontMeta, c: char) -> Option<Vec<u8>> {
    if !meta.simple || meta.embedded {
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

// ---------------------------------------------------------------------------
// New glyph runs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Piece {
    Glyph { code: Vec<u8>, em: f64, word_space: bool },
    /// Space without a glyph, in em.
    Gap(f64),
}

/// Text space advance of `pieces` in a text state.
fn pieces_advance(pieces: &[Piece], size: f64, tc: f64, tw: f64, th: f64) -> f64 {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Glyph { em, word_space, .. } => (em * size + tc + if *word_space { tw } else { 0.0 }) * th,
            Piece::Gap(em) => em * size * th,
        })
        .sum()
}

fn tokens_from_pieces(pieces: &[Piece]) -> Vec<Tok> {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Glyph { code, .. } => Tok::Code(code.clone()),
            Piece::Gap(em) => Tok::Kern(-em * 1000.0),
        })
        .collect()
}

/// Encodes `text` with an existing font of the page, if it is known to have every glyph.
fn encode_in_font(ctx: &Ctx, doc: &lopdf::Document, meta: &FontMeta, text: &str) -> Option<Vec<Piece>> {
    let learned = ctx.learned.get(&meta.key);
    let len = meta.code_len?;
    let width = |code: &[u8], c: char| -> f64 {
        if meta.has_widths {
            *meta.widths.get(&code_value(code)).unwrap_or(&meta.missing)
        } else {
            ctx.lib.advance(&meta.family, meta.bold, meta.italic, c).unwrap_or(0.5) as f64
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
    for c in text.chars() {
        let code = learned.and_then(|l| l.codes.get(&c).cloned()).or_else(|| encode_unembedded(doc, meta, c));
        match code {
            Some(code) => {
                let em = width(&code, c);
                out.push(Piece::Glyph { word_space: len == 1 && code == [32], em, code });
            }
            None if c == ' ' => out.push(Piece::Gap(gap)),
            None => return None,
        }
    }
    Some(out)
}

/// An installed font for text that the PDF's own font cannot show.
fn fallback_font(family: &str, base: &str, bold: bool, italic: bool, serif: bool, mono: bool, text: &str) -> Option<(String, Arc<FontData>)> {
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
        _ if tex => &["Latin Modern Roman", "CMU Serif"],
        "times new roman" | "times" => &["Liberation Serif", "Tinos", "Nimbus Roman", "TeX Gyre Termes", "FreeSerif"],
        "arial" | "helvetica" => &["Liberation Sans", "Arimo", "Nimbus Sans", "TeX Gyre Heros", "FreeSans"],
        "courier new" | "courier" => &["Liberation Mono", "Cousine", "Nimbus Mono PS", "FreeMono"],
        "calibri" => &["Carlito"],
        "cambria" => &["Caladea"],
        f if f.contains("computer modern") || f.starts_with("cm") || f.contains("latin modern") => &["Latin Modern Roman", "CMU Serif"],
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
    // Anything installed that has the glyphs (CJK, Arabic, …).
    let mut families = crate::metrics::installed_families();
    families.sort_by_key(|f| (!(f.contains("noto") || f.contains("dejavu")), f.len()));
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

/// A standalone font file for face `index` of a font collection.
fn standalone_sfnt(data: &[u8], index: u32) -> Option<Vec<u8>> {
    if data.get(0..4)? != b"ttcf" {
        return Some(data.to_vec());
    }
    let be32 = |o: usize| -> Option<u32> { Some(u32::from_be_bytes(data.get(o..o + 4)?.try_into().ok()?)) };
    let be16 = |o: usize| -> Option<u16> { Some(u16::from_be_bytes(data.get(o..o + 2)?.try_into().ok()?)) };
    if index >= be32(8)? {
        return None;
    }
    let off = be32(12 + 4 * index as usize)? as usize;
    let n = be16(off + 4)? as usize;
    let mut out = data.get(off..off + 12)?.to_vec();
    let mut body: Vec<u8> = Vec::new();
    let head_len = 12 + 16 * n;
    let mut records = Vec::with_capacity(16 * n);
    for i in 0..n {
        let r = off + 12 + 16 * i;
        let (o, l) = (be32(r + 8)? as usize, be32(r + 12)? as usize);
        records.extend_from_slice(data.get(r..r + 8)?);
        records.extend_from_slice(&((head_len + body.len()) as u32).to_be_bytes());
        records.extend_from_slice(&(l as u32).to_be_bytes());
        body.extend_from_slice(data.get(o..o + l)?);
        while !body.len().is_multiple_of(4) {
            body.push(0);
        }
    }
    out.extend(records);
    out.extend(body);
    Some(out)
}

/// Embeds `font` (Type0, Identity-H) for `chars`, reusing a copy embedded
/// earlier. Returns the font dictionary's object.
fn embed_font(doc: &mut lopdf::Document, font: &FontData, chars: &BTreeSet<char>) -> anyhow::Result<ObjectId> {
    let face = font.face().ok_or_else(|| anyhow!("unreadable font"))?;
    let ps_name: String = face
        .names()
        .into_iter()
        .find(|n| n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME)
        .and_then(|n| n.to_string())
        .or_else(|| crate::metrics::face_families(&face).into_iter().next())
        .unwrap_or_else(|| "Font".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let tag = format!("{ps_name}#{}", font.face_index());
    let upem = face.units_per_em() as f64;
    let scale = |v: f64| (v * 1000.0 / upem).round() as i64;

    // An earlier edit may have embedded this font already: extend it.
    let existing = doc.objects.iter().find_map(|(id, o)| {
        let d = o.as_dict().ok()?;
        (d.get(b"P2WFont").ok()?.as_str().ok()? == tag.as_bytes()).then_some(*id)
    });
    let mut all: BTreeSet<char> = chars.clone();
    if let Some(id) = existing {
        if let Ok(s) = doc.get_dictionary(id).and_then(|d| d.get(b"P2WChars")).and_then(|o| o.as_str()) {
            all.extend(String::from_utf8_lossy(s).chars());
        }
    }
    let mut gids: BTreeMap<u16, (char, i64)> = BTreeMap::new();
    for &c in &all {
        if let Some(g) = face.glyph_index(c) {
            let w = face.glyph_hor_advance(g).map(|a| scale(a as f64)).unwrap_or(1000);
            gids.entry(g.0).or_insert((c, w));
        }
    }
    let mut w = Vec::new();
    for (g, (_, wd)) in &gids {
        w.push(Object::Integer(*g as i64));
        w.push(Object::Array(vec![Object::Integer(*wd)]));
    }
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<(u16, char)> = gids.iter().map(|(g, (c, _))| (*g, *c)).collect();
    for chunk in entries.chunks(100) {
        cmap.push_str(&format!("{} beginbfchar\n", chunk.len()));
        for (g, c) in chunk {
            let hex: String = c.encode_utf16(&mut [0u16; 2]).iter().map(|u| format!("{u:04X}")).collect();
            cmap.push_str(&format!("<{g:04X}> <{hex}>\n"));
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    let mut to_unicode = Stream::new(Dictionary::new(), cmap.into_bytes());
    let _ = to_unicode.compress();
    let used: String = all.iter().collect();

    if let Some(id) = existing {
        let tu = doc.add_object(to_unicode);
        let desc_id = {
            let d = doc.get_dictionary_mut(id)?;
            d.set("ToUnicode", tu);
            d.set("P2WChars", Object::String(used.into_bytes(), StringFormat::Literal));
            d.get(b"DescendantFonts").ok().and_then(|a| a.as_array().ok()).and_then(|a| a.first()).and_then(|o| o.as_reference().ok())
        };
        if let Some(desc) = desc_id {
            doc.get_dictionary_mut(desc)?.set("W", Object::Array(w));
        }
        return Ok(id);
    }

    let data = standalone_sfnt(font.bytes(), font.face_index()).ok_or_else(|| anyhow!("unreadable font collection"))?;
    let mut file_dict = Dictionary::new();
    file_dict.set("Length1", data.len() as i64);
    let mut file = Stream::new(file_dict, data);
    let _ = file.compress();
    let file_id = doc.add_object(file);
    let bbox = face.global_bounding_box();
    let family = crate::metrics::face_families(&face).into_iter().next().unwrap_or_default();
    let mut flags = 32i64;
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
    fd.set("FontName", Object::Name(ps_name.clone().into_bytes()));
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
    fd.set("FontFile2", file_id);
    let fd_id = doc.add_object(fd);
    let mut info = Dictionary::new();
    info.set("Registry", Object::string_literal("Adobe"));
    info.set("Ordering", Object::string_literal("Identity"));
    info.set("Supplement", 0i64);
    let mut cid = Dictionary::new();
    cid.set("Type", Object::Name(b"Font".to_vec()));
    cid.set("Subtype", Object::Name(b"CIDFontType2".to_vec()));
    cid.set("BaseFont", Object::Name(ps_name.clone().into_bytes()));
    cid.set("CIDSystemInfo", info);
    cid.set("FontDescriptor", fd_id);
    cid.set("CIDToGIDMap", Object::Name(b"Identity".to_vec()));
    cid.set("DW", 1000i64);
    cid.set("W", Object::Array(w));
    let cid_id = doc.add_object(cid);
    let tu = doc.add_object(to_unicode);
    let mut t0 = Dictionary::new();
    t0.set("Type", Object::Name(b"Font".to_vec()));
    t0.set("Subtype", Object::Name(b"Type0".to_vec()));
    t0.set("BaseFont", Object::Name(ps_name.into_bytes()));
    t0.set("Encoding", Object::Name(b"Identity-H".to_vec()));
    t0.set("DescendantFonts", Object::Array(vec![Object::Reference(cid_id)]));
    t0.set("ToUnicode", tu);
    t0.set("P2WFont", Object::String(tag.into_bytes(), StringFormat::Literal));
    t0.set("P2WChars", Object::String(used.into_bytes(), StringFormat::Literal));
    Ok(doc.add_object(t0))
}

/// Pieces of `text` in an installed font (codes are glyph ids).
fn pieces_in(font: &FontData, text: &str) -> Vec<Piece> {
    let Some(face) = font.face() else { return Vec::new() };
    let upem = face.units_per_em() as f64;
    text.chars()
        .filter_map(|c| {
            let g = face.glyph_index(c)?;
            let em = face.glyph_hor_advance(g).unwrap_or(0) as f64 / upem;
            Some(Piece::Glyph { code: g.0.to_be_bytes().to_vec(), em, word_space: false })
        })
        .collect()
}

/// Makes `font_id` available to the page and returns its resource name.
fn add_page_font(doc: &mut lopdf::Document, page_id: ObjectId, font_id: ObjectId) -> anyhow::Result<Vec<u8>> {
    let mut res = page_resources(doc, page_id).unwrap_or_default();
    let mut fonts = res.get(b"Font").ok().map(|f| deref(doc, f)).and_then(|f| f.as_dict().ok()).cloned().unwrap_or_default();
    if let Some((name, _)) = fonts.iter().find(|(_, v)| matches!(v, Object::Reference(id) if *id == font_id)) {
        return Ok(name.clone());
    }
    let name = (1..).map(|i| format!("P2W{i}").into_bytes()).find(|n| !fonts.has(n)).unwrap_or_default();
    fonts.set(name.clone(), font_id);
    res.set("Font", fonts);
    doc.get_dictionary_mut(page_id)?.set("Resources", res);
    Ok(name)
}

// ---------------------------------------------------------------------------
// Writing operators
// ---------------------------------------------------------------------------

fn fmt(v: f64) -> String {
    let v = if v.abs() < 1e-7 { 0.0 } else { v };
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

fn fmt_mat(m: &Mat) -> String {
    m.iter().map(|v| fmt(*v)).collect::<Vec<_>>().join(" ")
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::from("<");
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s.push('>');
    s
}

/// A TJ operator for `toks`.
fn tj(toks: &[Tok]) -> String {
    let mut out = String::from("[");
    let mut run: Vec<u8> = Vec::new();
    for t in toks {
        match t {
            Tok::Code(c) => run.extend_from_slice(c),
            Tok::Kern(n) => {
                if !run.is_empty() {
                    out.push_str(&hex(&run));
                    run.clear();
                }
                out.push(' ');
                out.push_str(&fmt(*n));
                out.push(' ');
            }
        }
    }
    if !run.is_empty() {
        out.push_str(&hex(&run));
    }
    out.push_str("] TJ");
    out
}

fn write_name(raw: &[u8]) -> String {
    format!("/{}", String::from_utf8_lossy(raw))
}

/// Operators that set what `'` and `"` set before showing text.
fn quote_prefix(sh: &Show) -> String {
    match sh.kind {
        ShowKind::Quote => "T* ".into(),
        ShowKind::DQuote => format!("{} Tw {} Tc T* ", fmt(sh.tw), fmt(sh.tc)),
        _ => String::new(),
    }
}

/// Replacement for an operator whose glyphs are removed: it still moves the
/// text position as far as the original did, when something continues from there.
fn neutralize(sh: &Show) -> anyhow::Result<String> {
    let mut out = quote_prefix(sh);
    let init: f64 = sh
        .parts
        .iter()
        .map_while(|p| match p {
            Arg::Num(n) => Some(*n),
            Arg::Str(b) if b.is_empty() => Some(0.0),
            _ => None,
        })
        .sum();
    if let Some(a) = sh.advance {
        let unit = sh.unit();
        if unit.abs() < 1e-9 {
            if a.abs() > 1e-6 {
                bail!("cannot remove text drawn with a zero font size");
            }
        } else {
            out.push_str(&format!("[{}] TJ", fmt(init - a / unit)));
        }
    }
    Ok(out)
}

/// The operator's content as code and adjustment tokens.
fn show_tokens(sh: &Show, len: usize) -> Vec<Tok> {
    let mut toks = Vec::new();
    for p in &sh.parts {
        match p {
            Arg::Num(n) => toks.push(Tok::Kern(*n)),
            Arg::Str(b) => toks.extend(split_codes(b, len).into_iter().map(Tok::Code)),
            _ => {}
        }
    }
    toks
}

/// Text space advance of tokens in the operator's text state, when the widths are known.
fn tokens_advance(ctx: &Ctx, meta: &FontMeta, sh: &Show, toks: &[Tok], chars: &[char]) -> f64 {
    let mut adv = 0.0;
    let mut k = 0;
    for t in toks {
        match t {
            Tok::Code(c) => {
                let em = if meta.has_widths {
                    *meta.widths.get(&code_value(c)).unwrap_or(&meta.missing)
                } else {
                    chars.get(k).and_then(|ch| ctx.lib.advance(&meta.family, meta.bold, meta.italic, *ch)).unwrap_or(0.5) as f64
                };
                let ws = if c.as_slice() == [32] { sh.tw } else { 0.0 };
                adv += (em * sh.size + sh.tc + ws) * sh.th;
                k += 1;
            }
            Tok::Kern(n) => adv -= n / 1000.0 * sh.size * sh.th,
        }
    }
    adv
}

// ---------------------------------------------------------------------------
// Editing one page
// ---------------------------------------------------------------------------

fn edit_page(ctx: &mut Ctx, pdoc: &PdfDocument, doc: &mut lopdf::Document, page_ids: &[ObjectId], page: usize, edits: &[&TextEdit]) -> anyhow::Result<()> {
    let page_id = page_ids[page];
    let ppage = pdoc.pages().get(page as PdfPageIndex)?;
    let objs = page_objects(&ppage)?;
    let by_index: HashMap<usize, &PObj> = objs.iter().map(|o| (o.index, o)).collect();
    let content = doc.get_page_content(page_id);
    let pairs: Vec<(usize, (f64, f64))> = objs.iter().map(|o| (o.index, o.origin)).collect();
    let needs_scan = edits.iter().any(|e| !matches!(e, TextEdit::Insert { .. }));
    let scan = match scan_page(&content, &pairs) {
        Ok(s) => s,
        Err(e) if needs_scan => return Err(e.context("the text of this page cannot be edited")),
        Err(_) => {
            let ops = parse_content(&content).unwrap_or_default();
            let (mut depth, mut min_depth) = (0i64, 0i64);
            for op in &ops {
                match op.name.as_slice() {
                    b"q" => depth += 1,
                    b"Q" => {
                        depth -= 1;
                        min_depth = min_depth.min(depth);
                    }
                    _ => {}
                }
            }
            PageScan { ops, shows: Vec::new(), min_depth, end_depth: depth }
        }
    };
    let fonts = page_fonts(doc, page_id);
    if ctx.learned_pages.insert(page) {
        learn(ctx, &scan, &by_index, &fonts);
    }
    let show_of: HashMap<usize, usize> = scan.shows.iter().enumerate().map(|(k, s)| (s.object, k)).collect();
    let tf = PageTransform::new(&ppage);

    let mut splices: Vec<(usize, usize, String)> = Vec::new();
    let mut appended = String::new();
    let mut touched: BTreeSet<usize> = BTreeSet::new();

    for e in edits {
        match e {
            TextEdit::Delete { objects, .. } | TextEdit::Replace { objects, .. } => {
                let mut shows: Vec<usize> = Vec::new();
                for o in objects {
                    let k = *show_of.get(o).ok_or_else(|| anyhow!("text object {o} is not part of the page content"))?;
                    if !touched.insert(k) {
                        bail!("text object {o} is edited twice");
                    }
                    shows.push(k);
                }
                shows.sort_by_key(|k| scan.shows[*k].op);
                if shows.is_empty() {
                    continue;
                }
                let text = match e {
                    TextEdit::Replace { text, .. } => text.as_str(),
                    _ => "",
                };
                let fit = matches!(e, TextEdit::Replace { fit: true, .. });
                if text.trim().is_empty() {
                    for &k in &shows {
                        let sh = &scan.shows[k];
                        let op = &scan.ops[sh.op];
                        splices.push((op.start, op.end, neutralize(sh)?));
                    }
                    continue;
                }
                replace_run(ctx, pdoc, doc, page_ids, page, &scan, &fonts, &by_index, &shows, text, fit, &mut splices)?;
            }
            TextEdit::Insert { x, y, text, size, color, style, .. } => {
                if text.trim().is_empty() {
                    continue;
                }
                if text.chars().any(is_rtl) {
                    bail!(RTL_UNSUPPORTED);
                }
                let family = match style.family.to_ascii_lowercase().as_str() {
                    "" | "sans" | "sans-serif" => "Arial".to_string(),
                    "serif" => "Times New Roman".to_string(),
                    "mono" | "monospace" => "Courier New".to_string(),
                    _ => style.family.clone(),
                };
                let (used, font) = fallback_font(&family, "", style.bold, style.italic, family == "Times New Roman", family == "Courier New", text)
                    .ok_or_else(|| anyhow!("no installed font can show “{text}”"))?;
                let chars: BTreeSet<char> = text.chars().collect();
                let font_id = embed_font(doc, &font, &chars)?;
                let name = add_page_font(doc, page_id, font_id)?;
                let size = *size as f64;
                // Display coordinates → user space, upright as displayed.
                let (dw, dh) = (tf.width(), tf.height());
                let (px, py) = (x * dw, y * dh + 0.8 * size as f32);
                let o = tf.to_user(px, py);
                let r = tf.to_user(px + 1.0, py);
                let u = tf.to_user(px, py - 1.0);
                let m: Mat = [(r.0 - o.0) as f64, (r.1 - o.1) as f64, (u.0 - o.0) as f64, (u.1 - o.1) as f64, o.0 as f64, o.1 as f64];
                let toks = tokens_from_pieces(&pieces_in(&font, text));
                appended.push_str(&format!(
                    "q BT {} {} Tf {} {} {} rg {} Tm {} ET Q\n",
                    write_name(&name),
                    fmt(size),
                    fmt(color[0] as f64 / 255.0),
                    fmt(color[1] as f64 / 255.0),
                    fmt(color[2] as f64 / 255.0),
                    fmt_mat(&m),
                    tj(&toks)
                ));
                if !used.eq_ignore_ascii_case(&family) {
                    ctx.report.notes.push(format!("Page {}: new text uses {used}.", page + 1));
                }
            }
        }
    }

    // Rebuild the content.
    splices.sort_by_key(|s| s.0);
    let mut out: Vec<u8> = Vec::with_capacity(content.len() + 256);
    let wrap = !appended.is_empty();
    let k = if wrap { 1 + (-scan.min_depth).max(0) } else { 0 };
    for _ in 0..k {
        out.extend_from_slice(b"q\n");
    }
    let mut pos = 0;
    for (start, end, text) in &splices {
        if *start < pos {
            bail!("overlapping edits");
        }
        out.extend_from_slice(&content[pos..*start]);
        out.extend_from_slice(text.as_bytes());
        pos = *end;
    }
    out.extend_from_slice(&content[pos..]);
    if wrap {
        out.push(b'\n');
        for _ in 0..(k + scan.end_depth).max(0) {
            out.extend_from_slice(b"Q\n");
        }
        out.extend_from_slice(appended.as_bytes());
    }
    let mut stream = Stream::new(Dictionary::new(), out);
    let _ = stream.compress();
    let id = doc.add_object(stream);
    doc.get_dictionary_mut(page_id)?.set("Contents", id);
    Ok(())
}

/// Replaces the glyphs of `shows` (one run, in content order) with `text`.
#[allow(clippy::too_many_arguments)]
fn replace_run(
    ctx: &mut Ctx,
    pdoc: &PdfDocument,
    doc: &mut lopdf::Document,
    page_ids: &[ObjectId],
    page: usize,
    scan: &PageScan,
    fonts: &HashMap<Vec<u8>, FontMeta>,
    objs: &HashMap<usize, &PObj>,
    shows: &[usize],
    text: &str,
    fit: bool,
    splices: &mut Vec<(usize, usize, String)>,
) -> anyhow::Result<()> {
    let page_id = page_ids[page];
    let first = &scan.shows[shows[0]];
    let rtl_before = shows.iter().filter_map(|k| objs.get(&scan.shows[*k].object)).any(|o| o.seq.iter().any(|(c, _)| is_rtl(*c)));
    if text.chars().any(is_rtl) || rtl_before {
        bail!(RTL_UNSUPPORTED);
    }
    let meta = fonts.get(&decode_name(&first.font));
    let pobj = objs.get(&first.object).copied();

    // Learn the font's glyphs from the rest of the document when this page does not show them all.
    if let Some(meta) = meta {
        let chars: BTreeSet<char> = text.chars().filter(|c| *c != ' ').collect();
        let missing = chars.iter().any(|c| !ctx.learned.get(&meta.key).is_some_and(|l| l.codes.contains_key(c)));
        if missing && meta.embedded {
            learn_more(ctx, pdoc, doc, page_ids, &meta.key, &chars);
        }
    }

    // One operator: keep the unchanged start and end of the text exactly as they were.
    if shows.len() == 1 {
        if let (Some(meta), Some(o), Some(len)) = (meta, pobj, meta.and_then(|m| m.code_len)) {
            let toks = show_tokens(first, len);
            let n_codes = toks.iter().filter(|t| matches!(t, Tok::Code(_))).count();
            let real: Vec<char> = o.seq.iter().filter(|(_, g)| !g).map(|(c, _)| *c).collect();
            if n_codes == real.len() {
                let old: Vec<char> = o.seq.iter().map(|(c, _)| *c).collect();
                let new: Vec<char> = text.chars().collect();
                let mut p = 0;
                while p < old.len() && p < new.len() && old[p] == new[p] {
                    p += 1;
                }
                let mut sfx = 0;
                while sfx < old.len() - p && sfx < new.len() - p && old[old.len() - 1 - sfx] == new[new.len() - 1 - sfx] {
                    sfx += 1;
                }
                if p == old.len() && p == new.len() {
                    return Ok(());
                }
                let codes_before = |n: usize| o.seq[..n].iter().filter(|(_, g)| !g).count();
                let cp = codes_before(p);
                let cs = n_codes - codes_before(old.len() - sfx);
                let code_pos: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| matches!(t, Tok::Code(_))).map(|(i, _)| i).collect();
                let mut pre_end = if cp == 0 { code_pos.first().copied().unwrap_or(toks.len()) } else { code_pos[cp - 1] + 1 };
                if p > 0 && o.seq[p - 1].1 {
                    while pre_end < toks.len() && matches!(toks[pre_end], Tok::Kern(_)) {
                        pre_end += 1;
                    }
                }
                let mut suf_start = if cs == 0 { code_pos.last().map(|i| i + 1).unwrap_or(toks.len()) } else { code_pos[n_codes - cs] };
                if sfx > 0 && o.seq[old.len() - sfx].1 {
                    while suf_start > pre_end && matches!(toks[suf_start - 1], Tok::Kern(_)) {
                        suf_start -= 1;
                    }
                }
                let suf_start = suf_start.max(pre_end);
                let middle: String = new[p..new.len() - sfx].iter().collect();
                let (prefix, suffix) = (&toks[..pre_end], &toks[suf_start..]);
                let op = &scan.ops[first.op];
                let old_adv = tokens_advance(ctx, meta, first, &toks, &real);
                if let Some(pieces) = encode_in_font(ctx, doc, meta, &middle) {
                    let mut all: Vec<Tok> = prefix.to_vec();
                    all.extend(tokens_from_pieces(&pieces));
                    all.extend_from_slice(suffix);
                    let real_kept: Vec<char> = real[..cp].iter().chain(middle.chars().filter(|c| *c != ' ').collect::<Vec<_>>().iter()).chain(real[real.len() - cs..].iter()).copied().collect();
                    let new_adv = tokens_advance(ctx, meta, first, &all, &real_kept);
                    let mut s = quote_prefix(first);
                    let th = fit_scale(fit, first.th, old_adv, new_adv);
                    if let Some(th) = th {
                        s.push_str(&format!("{} Tz ", fmt(th * 100.0)));
                    }
                    s.push_str(&tj(&all));
                    if th.is_some() {
                        s.push_str(&format!(" {} Tz", fmt(first.th * 100.0)));
                    }
                    splices.push((op.start, op.end, s));
                    return Ok(());
                }
                // The changed characters in an installed font, between the original start and end.
                let (fam, f) = fallback_font(&meta.family, &meta.base, meta.bold, meta.italic, o.serif, o.mono, &middle)
                    .ok_or_else(|| anyhow!("no installed font can show “{middle}”"))?;
                note_substitution(ctx, page, &meta.base, &fam);
                let chars: BTreeSet<char> = middle.chars().collect();
                let font_id = embed_font(doc, &f, &chars)?;
                let name = add_page_font(doc, page_id, font_id)?;
                let pieces = pieces_in(&f, &middle);
                let new_adv = tokens_advance(ctx, meta, first, prefix, &real[..cp])
                    + pieces_advance(&pieces, first.size, first.tc, 0.0, first.th)
                    + tokens_advance(ctx, meta, first, suffix, &real[real.len() - cs..]);
                let th = fit_scale(fit, first.th, old_adv, new_adv);
                let mut s = quote_prefix(first);
                if let Some(th) = th {
                    s.push_str(&format!("{} Tz ", fmt(th * 100.0)));
                }
                s.push_str(&tj(prefix));
                s.push_str(&format!(" {} {} Tf {}", write_name(&name), fmt(first.size), tj(&tokens_from_pieces(&pieces))));
                s.push_str(&format!(" {} {} Tf {}", write_name(&first.font), fmt(first.size), tj(suffix)));
                if th.is_some() {
                    s.push_str(&format!(" {} Tz", fmt(first.th * 100.0)));
                }
                splices.push((op.start, op.end, s));
                return Ok(());
            }
        }
    }

    // Several operators (or an operator whose characters cannot be matched to
    // its codes): the whole run is drawn again where its first glyph was.
    let (pieces, font_switch) = match meta.and_then(|m| encode_in_font(ctx, doc, m, text)) {
        Some(p) => (p, None),
        None => {
            let (family, base, bold, italic) =
                meta.map(|m| (m.family.clone(), m.base.clone(), m.bold, m.italic)).unwrap_or_else(|| ("Arial".into(), String::new(), false, false));
            let (serif, mono) = pobj.map(|o| (o.serif, o.mono)).unwrap_or((false, false));
            let (fam, f) = fallback_font(&family, &base, bold, italic, serif, mono, text).ok_or_else(|| anyhow!("no installed font can show “{text}”"))?;
            note_substitution(ctx, page, if base.is_empty() { &family } else { &base }, &fam);
            let chars: BTreeSet<char> = text.chars().collect();
            let font_id = embed_font(doc, &f, &chars)?;
            let name = add_page_font(doc, page_id, font_id)?;
            (pieces_in(&f, text), Some(name))
        }
    };
    let tw = if font_switch.is_some() { 0.0 } else { first.tw };
    let new_adv = pieces_advance(&pieces, first.size, first.tc, tw, first.th);
    // The original width along the baseline, from PDFium's bounds.
    let old_adv = pobj.map(|o| {
        let scale = (o.m[0] * o.m[0] + o.m[1] * o.m[1]).sqrt();
        let mut ext = 0.0f64;
        for k in shows {
            if let Some(oo) = objs.get(&scan.shows[*k].object) {
                ext = ext.max(extent(o, oo));
            }
        }
        // `scale` is page units per text space unit.
        if scale > 1e-9 {
            ext / scale
        } else {
            0.0
        }
    });
    let th = old_adv.and_then(|w| fit_scale(fit, first.th, w, new_adv));
    let op = &scan.ops[first.op];
    let mut s = String::new();
    if first.kind == ShowKind::DQuote {
        s.push_str(&format!("{} Tw {} Tc ", fmt(first.tw), fmt(first.tc)));
    }
    s.push_str(&format!("ET BT {} Tm ", fmt_mat(&first.tm())));
    if let Some(name) = &font_switch {
        s.push_str(&format!("{} {} Tf ", write_name(name), fmt(first.size)));
    }
    if let Some(th) = th {
        s.push_str(&format!("{} Tz ", fmt(th * 100.0)));
    }
    s.push_str(&tj(&tokens_from_pieces(&pieces)));
    if font_switch.is_some() {
        s.push_str(&format!(" {} {} Tf", write_name(&first.font), fmt(first.size)));
    }
    if th.is_some() {
        s.push_str(&format!(" {} Tz", fmt(first.th * 100.0)));
    }
    s.push_str(&format!(" ET BT {} Tm", fmt_mat(&first.tlm)));
    if let Some(a) = first.advance {
        let unit = first.unit();
        if unit.abs() > 1e-9 {
            s.push_str(&format!(" [{}] TJ", fmt(-(first.s + a) / unit)));
        }
    }
    splices.push((op.start, op.end, s));
    for &k in &shows[1..] {
        let sh = &scan.shows[k];
        let op = &scan.ops[sh.op];
        splices.push((op.start, op.end, neutralize(sh)?));
    }
    Ok(())
}

/// Horizontal scaling that keeps `new_adv` within `old_adv`, when asked to fit.
fn fit_scale(fit: bool, th: f64, old_adv: f64, new_adv: f64) -> Option<f64> {
    if !fit || old_adv <= 0.0 || new_adv <= old_adv * 1.01 {
        return None;
    }
    Some(th * (old_adv / new_adv).max(0.5))
}

fn note_substitution(ctx: &mut Ctx, page: usize, original: &str, used: &str) {
    // The same family (e.g. a font embedded by an earlier edit) is no substitution.
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    if norm(original).starts_with(&norm(used)) {
        return;
    }
    let note = format!("Page {}: “{original}” in this PDF lacks some of the new characters; they use {used} instead.", page + 1);
    if !ctx.report.notes.contains(&note) {
        ctx.report.notes.push(note);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexes_operators_and_operands() {
        let d = b"q 1 0 0 1 72 700 cm BT /F1 12 Tf [(Hel\\)lo) -250 <0041 42>] TJ (a\\053b) Tj ET\nBI /W 2 /H 1 ID \x00\xff\nEI Q % done\n";
        let ops = parse_content(d).unwrap();
        let names: Vec<&[u8]> = ops.iter().map(|o| o.name.as_slice()).collect();
        assert_eq!(names, vec![&b"q"[..], b"cm", b"BT", b"Tf", b"TJ", b"Tj", b"ET", b"BI", b"Q"]);
        assert_eq!(ops[4].args, vec![Arg::Array(vec![Arg::Str(b"Hel)lo".to_vec()), Arg::Num(-250.0), Arg::Str(vec![0x00, 0x41, 0x42])])]);
        assert_eq!(ops[5].args, vec![Arg::Str(b"a+b".to_vec())]);
        assert_eq!(&d[ops[4].start..ops[4].end], b"[(Hel\\)lo) -250 <0041 42>] TJ");
    }

    #[test]
    fn follows_text_position_and_measures_advances() {
        // Two operators on one line: the second continues where the first ended.
        let d = b"BT /F1 10 Tf 100 700 Td (ab) Tj [-100 (cd)] TJ 0 -12 Td (ef) Tj ET";
        // PDFium would put "cd" at 100 + width(ab) + 1 (the -100 adjustment at size 10).
        let objs = vec![(0, (100.0, 700.0)), (1, (100.0 + 11.0 + 1.0, 700.0)), (2, (100.0, 688.0))];
        let scan = scan_page(d, &objs).unwrap();
        assert_eq!(scan.shows.len(), 3);
        assert!((scan.shows[0].advance.unwrap() - 11.0).abs() < 1e-6);
        assert!(scan.shows[1].advance.is_none());
        assert!(scan.shows[2].advance.is_none());
        // Removing "ab" keeps "cd" in place.
        assert_eq!(neutralize(&scan.shows[0]).unwrap(), "[-1100] TJ");
        // A wrong origin is detected.
        let bad = vec![(0, (100.0, 700.0)), (1, (112.0, 700.0)), (2, (100.0, 600.0))];
        assert!(scan_page(d, &bad).is_err());
    }

    #[test]
    fn names_and_numbers() {
        assert_eq!(decode_name(b"F#201"), b"F 1".to_vec());
        assert_eq!(parse_number(b"--1.5"), -1.5);
        assert_eq!(fmt(-0.00001), "0");
        assert_eq!(fmt(12.5), "12.5");
        assert_eq!(tj(&[Tok::Code(vec![65]), Tok::Kern(-250.0), Tok::Code(vec![66, 67])]), "[<41> -250 <4243>] TJ");
    }
}
