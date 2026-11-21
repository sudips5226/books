//! Editing the text of a PDF.
//!
//! PDFium finds the text of a page (objects, also inside form XObjects, with
//! their fonts, positions and characters); runs, lines and paragraphs are
//! built from them. The edits are made in the content streams: only the
//! operators that draw the edited text are rewritten, and everything else
//! stays byte for byte as it was. (PDFium can regenerate a page's content,
//! but it drops the kerning of every other line on the page while doing so.)
//!
//! New text keeps the PDF's own font when the font is known to have the
//! glyphs: learned from the characters it draws in the document, read from
//! its embedded program, or any character of a font that is not embedded.
//! Otherwise a matching installed font is shaped (kerning, ligatures,
//! right-to-left and complex scripts) and embedded as a subset.
//!
//! Edits are saved as incremental updates (fast for big files); [`compact`]
//! rewrites a file without its earlier revisions.

mod content;
mod flow;
mod fonts;
mod objects;
mod tables;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{anyhow, bail, Context};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use pdfium_render::prelude::*;

use content::{apply, apply_vec, fmt, fmt_mat, invert, mul, neutralize, quote_prefix, resume, tj_ops, translate, write_name, x_scale, Mat, Scan, Show, Tok, IDENTITY};
use fonts::{deref, encode_in_font, fallback_pieces, fallback_text, installed_variant, is_rtl, needs_shaping, pieces_advance, resource_fonts, split_codes, Ctx, FontMeta, Piece};
use objects::{group_lines, group_lines_of, group_paragraphs, group_runs, page_objects, run_geom, run_text, Line, PObj, PageObjects, Para, RunGeom, Src};

pub use objects::{Align, ObjPath};
pub use tables::{TableCell, TableInfo};

use crate::extract::PageTransform;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A run of text on a page (same font, one line) that can be edited as a whole.
#[derive(Clone, Debug)]
pub struct TextItem {
    /// The page objects (as PDFium numbers them) that make up the run.
    pub objects: Vec<ObjPath>,
    pub text: String,
    /// Bounding box as fractions of the displayed page (0..1, origin top-left).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Font name (subset prefix removed).
    pub font: String,
    /// Font size in points as it appears on the page.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    /// Invisible text, e.g. the text layer of an OCRed scan.
    pub invisible: bool,
    /// Right-to-left text.
    pub rtl: bool,
    /// Index of the paragraph (in [`PageText::paragraphs`]).
    pub paragraph: usize,
    /// Baseline of the first line (fraction of the displayed page height).
    pub baseline: f32,
    /// Letters drawn as vector outlines, recognised with OCR (see [`outlined_text`]).
    pub outlined: bool,
}

/// A paragraph: runs on consecutive lines of one column.
#[derive(Clone, Debug)]
pub struct ParagraphInfo {
    /// Indices into [`PageText::items`].
    pub items: Vec<usize>,
    /// The text, lines joined by spaces (and words split by a hyphen joined again).
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub align: Align,
    pub lines: usize,
}

/// The editable text of a page.
#[derive(Clone, Debug)]
pub struct PageText {
    /// Size of the displayed page in points.
    pub width: f32,
    pub height: f32,
    pub items: Vec<TextItem>,
    pub paragraphs: Vec<ParagraphInfo>,
    /// Shapes on the page that look like letters turned into outlines (not
    /// text): [`outlined_text`] can recognise them.
    pub outlined_shapes: usize,
    pub images: Vec<ImageItem>,
    /// Data tables, with their cells ([`TextEdit::Cell`] edits a cell).
    pub tables: Vec<TableInfo>,
}

/// A picture on a page.
#[derive(Clone, Debug)]
pub struct ImageItem {
    pub path: ObjPath,
    /// Box as fractions of the displayed page (0..1, origin top-left).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// A change to a picture.
#[derive(Clone, Debug)]
pub enum ImageChange {
    Delete,
    /// Moves and scales the picture into this box (fractions of the displayed page).
    Place { x: f32, y: f32, w: f32, h: f32 },
    /// Shows another picture (PNG, JPEG …) in the same place.
    Replace { file: std::path::PathBuf },
}

/// Style of newly added text.
#[derive(Clone, Debug, Default)]
pub struct NewTextStyle {
    /// Font family; empty for a sans-serif default. "serif" and "mono" pick generic families.
    pub family: String,
    pub bold: bool,
    pub italic: bool,
}

/// Changes to the look of a run (`None` keeps the current value).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleChange {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    /// Font size in points as it appears on the page.
    pub size: Option<f32>,
    pub color: Option<[u8; 3]>,
}

/// One change to a PDF's text. Pages are 0-based; objects are [`TextItem::objects`].
#[derive(Clone, Debug)]
pub enum TextEdit {
    /// Replaces the text of a run. With `fit`, text that would be wider than the
    /// original is condensed to the original width.
    Replace { page: usize, objects: Vec<ObjPath>, text: String, fit: bool },
    /// Removes a run (the space it took stays empty).
    Delete { page: usize, objects: Vec<ObjPath> },
    /// Adds text whose top-left corner is at (`x`, `y`), fractions of the displayed page.
    Insert { page: usize, x: f32, y: f32, text: String, size: f32, color: [u8; 3], style: NewTextStyle },
    /// Changes the style of a run, and its text when `text` is given.
    Restyle { page: usize, objects: Vec<ObjPath>, text: Option<String>, change: StyleChange, fit: bool },
    /// Moves a run by (`dx`, `dy`), fractions of the displayed page.
    Move { page: usize, objects: Vec<ObjPath>, dx: f32, dy: f32 },
    /// Replaces (or with empty `text`, removes) letters drawn as vector
    /// outlines inside the box (`x`, `y`, `w`, `h`, fractions of the displayed
    /// page), writing `text` on `baseline` with an installed font.
    Outlined { page: usize, x: f32, y: f32, w: f32, h: f32, baseline: f32, text: String, size: f32, color: [u8; 3], style: NewTextStyle, fit: bool },
    /// Permanently removes everything in a box (fractions of the displayed
    /// page) — text, shapes, the pixels of pictures, annotations — and fills
    /// it with `fill` (black when `None`).
    Redact { page: usize, x: f32, y: f32, w: f32, h: f32, fill: Option<[u8; 3]> },
    /// Changes a picture ([`PageText::images`]).
    Image { page: usize, path: ObjPath, change: ImageChange },
    /// Adds a picture into a box (fractions of the displayed page).
    InsertImage { page: usize, x: f32, y: f32, w: f32, h: f32, file: std::path::PathBuf },
    /// Rewrites a paragraph (all its runs) with new text, wrapping it over as
    /// many lines as it needs. With `fit` the text is made smaller instead of
    /// taking more lines; with `push_down` the text below moves down when the
    /// paragraph grows; with `flow` lines that no longer fit in the column
    /// move on to the next column and page (pages are added when needed).
    /// A `\n` in `text` starts a new line.
    Paragraph { page: usize, objects: Vec<ObjPath>, text: String, fit: bool, push_down: bool, flow: bool },
    /// Rewrites the text of a table cell ([`TableCell`]), wrapping it inside
    /// the cell's box (fractions of the displayed page). With `fit` the text
    /// is made smaller when it would not fit the cell. A `\n` starts a new line.
    Cell { page: usize, x: f32, y: f32, w: f32, h: f32, objects: Vec<ObjPath>, text: String, fit: bool },
}

impl TextEdit {
    fn page(&self) -> usize {
        match self {
            TextEdit::Replace { page, .. }
            | TextEdit::Delete { page, .. }
            | TextEdit::Insert { page, .. }
            | TextEdit::Restyle { page, .. }
            | TextEdit::Move { page, .. }
            | TextEdit::Outlined { page, .. }
            | TextEdit::Redact { page, .. }
            | TextEdit::Image { page, .. }
            | TextEdit::InsertImage { page, .. }
            | TextEdit::Paragraph { page, .. }
            | TextEdit::Cell { page, .. } => *page,
        }
    }
}

/// What happened while applying edits.
#[derive(Clone, Debug, Default)]
pub struct EditReport {
    /// Font substitutions and similar remarks, for the user.
    pub notes: Vec<String>,
}

/// How text is searched.
#[derive(Clone, Copy, Debug, Default)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_word: bool,
}

/// An occurrence of searched text (character offsets in the paragraph's text).
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub page: usize,
    pub paragraph: usize,
    pub start: usize,
    pub end: usize,
    /// Some text before and after, for showing the match.
    pub before: String,
    pub found: String,
    pub after: String,
}

/// The editable text of a page (0-based).
pub fn page_text(input: &Path, password: Option<&str>, page: usize, pdfium_path: Option<&Path>) -> anyhow::Result<PageText> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let p = document.pages().get(page as PdfPageIndex)?;
    let a = analyse(&p)?;
    let mut pt = a.page_text();
    pt.tables = tables::find_tables(&document, &p, page, &a, &pt);
    Ok(pt)
}

/// Letters drawn as vector outlines on a page (0-based), recognised with
/// OCR (`language`: Tesseract languages such as "eng+deu", or "auto").
/// Edit them with [`TextEdit::Outlined`].
pub fn outlined_text(input: &Path, password: Option<&str>, page: usize, language: &str, pdfium_path: Option<&Path>) -> anyhow::Result<Vec<TextItem>> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let p = document.pages().get(page as PdfPageIndex)?;
    let a = analyse(&p)?;
    let shapes = a.outline_shapes();
    if shapes.is_empty() {
        return Ok(Vec::new());
    }
    let (pw, ph) = (a.tf.width() as f64, a.tf.height() as f64);
    let img = a.tf.render_with(&p, 300.0 / 72.0, false)?;
    let dpi = img.width() as f32 / pw as f32 * 72.0;
    let installed = crate::scan::available_languages(None);
    let language = match language.trim() {
        "" | "auto" => {
            if installed.is_empty() || installed.iter().any(|l| l == "eng") {
                "eng".to_string()
            } else {
                installed[0].clone()
            }
        }
        l => l.to_string(),
    };
    let settings = crate::ocr::OcrSettings { language, ..Default::default() };
    let mut engine = crate::ocr::OcrEngine::new(settings.clone())?;
    let mut words = engine.recognize(&img, dpi)?;
    // Only words made of outline shapes (not real text, pictures or lines).
    words.retain(|w| {
        let b = &w.bbox;
        let area = (b.width() * b.height()).max(1.0) as f64;
        let covered: f64 = shapes
            .iter()
            .map(|s| {
                let ix = (s.2.min(b.x1 as f64) - s.0.max(b.x0 as f64)).max(0.0);
                let iy = (s.3.min(b.y1 as f64) - s.1.max(b.y0 as f64)).max(0.0);
                ix * iy
            })
            .sum();
        covered / area > 0.25
    });
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut raw = crate::model::RawPage { width: pw as f32, height: ph as f32, ..Default::default() };
    crate::ocr::words_to_glyphs(&mut raw, &words, &img, &settings);
    let mut lines: BTreeMap<(u32, u32, u32), Vec<&crate::ocr::OcrWord>> = BTreeMap::new();
    for w in &words {
        lines.entry(w.line_key).or_default().push(w);
    }
    let mut items = Vec::new();
    for ws in lines.values_mut() {
        ws.sort_by(|a, b| a.bbox.x0.total_cmp(&b.bbox.x0));
        let text = ws.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        let mut r = ws[0].bbox;
        for w in ws.iter() {
            r = r.union(&w.bbox);
        }
        let glyph = raw.glyphs.iter().find(|g| r.expand(1.0).contains_point(g.bbox.cx(), g.bbox.cy()));
        let style = glyph.and_then(|g| raw.styles.get(g.style)).cloned().unwrap_or_default();
        let size = if style.size > 0.0 { style.size } else { r.height() * 0.75 };
        let baseline = glyph.map(|g| g.baseline).unwrap_or(r.y1 - 0.2 * size);
        let family = match style.generic {
            crate::model::Generic::Serif => "serif",
            crate::model::Generic::Mono => "monospace",
            _ => "sans-serif",
        };
        items.push(TextItem {
            objects: Vec::new(),
            text,
            x: r.x0 / pw as f32,
            y: r.y0 / ph as f32,
            w: r.width() / pw as f32,
            h: r.height() / ph as f32,
            font: format!("Outlined text ({family})"),
            size,
            bold: style.bold,
            italic: style.italic,
            color: [style.color.0, style.color.1, style.color.2],
            invisible: false,
            rtl: objects::first_strong_rtl(&ws.iter().map(|w| w.text.as_str()).collect::<String>()),
            paragraph: usize::MAX,
            baseline: baseline / ph as f32,
            outlined: true,
        });
    }
    Ok(items)
}

/// The editable text runs of a page (0-based).
pub fn text_items(input: &Path, password: Option<&str>, page: usize, pdfium_path: Option<&Path>) -> anyhow::Result<Vec<TextItem>> {
    Ok(page_text(input, password, page, pdfium_path)?.items)
}

/// Applies `edits` to `input` and writes the result to `output` (a different
/// file) as an incremental update of the original.
pub fn apply_edits(input: &Path, password: Option<&str>, edits: &[TextEdit], output: &Path, pdfium_path: Option<&Path>) -> anyhow::Result<EditReport> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let bytes = std::fs::read(input).with_context(|| format!("could not read {}", input.display()))?;
    let pdoc = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let prev = load_lopdf(&bytes, password)?;
    let mut s = Session::new(&pdoc, prev.clone())?;
    let mut by_page: BTreeMap<usize, Vec<&TextEdit>> = BTreeMap::new();
    for e in edits {
        if e.page() >= s.page_ids.len() {
            bail!("page {} does not exist", e.page() + 1);
        }
        by_page.entry(e.page()).or_default().push(e);
    }
    // Pages in order; text flowing on from a page is placed on the next one.
    let mut page = by_page.keys().next().copied().unwrap_or(0);
    while page < s.page_ids.len().min(s.pdoc.pages().len() as usize) {
        match by_page.remove(&page) {
            Some(list) => s.edit_page(page, &list).with_context(|| format!("page {}", page + 1))?,
            None if s.carry.is_some() => s.edit_page(page, &[]).with_context(|| format!("page {}", page + 1))?,
            None => match by_page.keys().next() {
                Some(p) => {
                    page = *p;
                    continue;
                }
                None => break,
            },
        }
        page += 1;
    }
    s.flow_new_pages()?;
    let report = EditReport { notes: s.ctx.notes.clone() };
    let doc = s.finish()?;
    save_incremental(bytes, prev, doc, output)?;
    drop(pdoc);
    pdfium.load_pdf_from_file(output, password).map_err(|e| anyhow!("the edited PDF could not be read back ({e})"))?;
    Ok(report)
}

/// Rewrites a PDF without the earlier revisions kept by incremental updates
/// (so that removed text is really gone from the file).
pub fn compact(input: &Path, password: Option<&str>, output: &Path, pdfium_path: Option<&Path>) -> anyhow::Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("could not read {}", input.display()))?;
    let mut doc = load_lopdf(&bytes, password)?;
    doc.prune_objects();
    doc.save(output).with_context(|| format!("could not write {}", output.display()))?;
    let pdfium = crate::load_pdfium(pdfium_path)?;
    pdfium.load_pdf_from_file(output, password).map_err(|e| anyhow!("the compacted PDF could not be read back ({e})"))?;
    Ok(())
}

/// Finds `query` in the document's paragraphs (also across line breaks).
pub fn find_text(input: &Path, password: Option<&str>, query: &str, opts: FindOptions, pdfium_path: Option<&Path>) -> anyhow::Result<Vec<Match>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let mut out = Vec::new();
    for (page, p) in document.pages().iter().enumerate() {
        let a = analyse(&p)?;
        for (pi, para) in a.paras.iter().enumerate() {
            for (start, end) in find_in(&para.text, query, opts) {
                let chars: Vec<char> = para.text.chars().collect();
                let b = start.saturating_sub(30);
                let e = (end + 30).min(chars.len());
                out.push(Match {
                    page,
                    paragraph: pi,
                    start,
                    end,
                    before: chars[b..start].iter().collect(),
                    found: chars[start..end].iter().collect(),
                    after: chars[end..e].iter().collect(),
                });
            }
        }
    }
    Ok(out)
}

/// Replaces the given matches (from [`find_text`] on the same file) with `replacement`.
pub fn replace_matches(
    input: &Path,
    password: Option<&str>,
    matches: &[Match],
    replacement: &str,
    output: &Path,
    pdfium_path: Option<&Path>,
) -> anyhow::Result<EditReport> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let mut edits = Vec::new();
    let mut pages: BTreeMap<usize, Vec<&Match>> = BTreeMap::new();
    for m in matches {
        pages.entry(m.page).or_default().push(m);
    }
    for (page, ms) in pages {
        let p = document.pages().get(page as PdfPageIndex)?;
        let a = analyse(&p)?;
        // Per run: characters removed, and text inserted before a character.
        let mut removed: HashMap<usize, HashSet<usize>> = HashMap::new();
        let mut inserted: HashMap<usize, BTreeMap<usize, String>> = HashMap::new();
        for m in ms {
            let Some(para) = a.paras.get(m.paragraph) else { continue };
            let mut first = true;
            for pos in m.start..m.end.min(para.src.len()) {
                if let Src::Run(r, i) = para.src[pos] {
                    removed.entry(r).or_default().insert(i);
                    if first {
                        inserted.entry(r).or_default().insert(i, replacement.to_string());
                        first = false;
                    }
                }
            }
            for &(pos, r, i) in &para.hyphens {
                if pos > m.start && pos < m.end {
                    removed.entry(r).or_default().insert(i);
                }
            }
        }
        let mut runs: Vec<usize> = removed.keys().copied().collect();
        runs.sort_unstable();
        for r in runs {
            let mut text = String::new();
            for (i, c) in a.texts[r].chars().enumerate() {
                if let Some(s) = inserted.get(&r).and_then(|m| m.get(&i)) {
                    text.push_str(s);
                }
                if !removed[&r].contains(&i) {
                    text.push(c);
                }
            }
            let objects = a.run_paths(r);
            let fit = inserted.contains_key(&r);
            edits.push(if text.trim().is_empty() { TextEdit::Delete { page, objects } } else { TextEdit::Replace { page, objects, text, fit } });
        }
    }
    drop(document);
    if edits.is_empty() {
        std::fs::copy(input, output)?;
        return Ok(EditReport::default());
    }
    apply_edits(input, password, &edits, output, pdfium_path)
}

/// Replaces every occurrence of `find`. Returns the number of occurrences replaced.
pub fn find_replace(
    input: &Path,
    password: Option<&str>,
    find: &str,
    replace: &str,
    opts: FindOptions,
    output: &Path,
    pdfium_path: Option<&Path>,
) -> anyhow::Result<(usize, EditReport)> {
    if find.is_empty() {
        bail!("nothing to find");
    }
    let matches = find_text(input, password, find, opts, pdfium_path)?;
    if matches.is_empty() {
        return Ok((0, EditReport::default()));
    }
    let report = replace_matches(input, password, &matches, replace, output, pdfium_path)?;
    Ok((matches.len(), report))
}

/// Redacts the given matches (from [`find_text`] on the same file): their
/// characters are removed for good and covered with `fill` (black when `None`).
pub fn redact_matches(input: &Path, password: Option<&str>, matches: &[Match], fill: Option<[u8; 3]>, output: &Path, pdfium_path: Option<&Path>) -> anyhow::Result<EditReport> {
    let pdfium = crate::load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let mut edits = Vec::new();
    let mut pages: BTreeMap<usize, Vec<&Match>> = BTreeMap::new();
    for m in matches {
        pages.entry(m.page).or_default().push(m);
    }
    for (page, ms) in pages {
        let p = document.pages().get(page as PdfPageIndex)?;
        let a = analyse(&p)?;
        let (pw, ph) = (a.tf.width() as f64, a.tf.height() as f64);
        for m in ms {
            let Some(para) = a.paras.get(m.paragraph) else { continue };
            // Character boxes of the match, one box per text object.
            let mut boxes: BTreeMap<usize, [f64; 4]> = BTreeMap::new();
            for pos in m.start..m.end.min(para.src.len()) {
                let Src::Run(r, i) = para.src[pos] else { continue };
                let Some((k, ci)) = objects::run_char_map(&a.po.objs, &a.runs[r]).get(i).copied().flatten() else { continue };
                let b = a.po.objs[k].boxes[ci];
                let e = boxes.entry(k).or_insert(b);
                *e = [e[0].min(b[0]), e[1].min(b[1]), e[2].max(b[2]), e[3].max(b[3])];
            }
            for b in boxes.values() {
                let r = disp_rect(&a.tf, *b);
                let pad = 0.5;
                edits.push(TextEdit::Redact {
                    page,
                    x: ((r.0 - pad) / pw) as f32,
                    y: ((r.1 - pad) / ph) as f32,
                    w: ((r.2 - r.0 + 2.0 * pad) / pw) as f32,
                    h: ((r.3 - r.1 + 2.0 * pad) / ph) as f32,
                    fill,
                });
            }
        }
    }
    drop(document);
    if edits.is_empty() {
        std::fs::copy(input, output)?;
        return Ok(EditReport::default());
    }
    apply_edits(input, password, &edits, output, pdfium_path)
}

/// Width and height in pixels of a picture file, if it can be read.
pub fn image_size(file: &Path) -> Option<(u32, u32)> {
    image::image_dimensions(file).ok()
}

/// A page-space rectangle (left, bottom, right, top) on the display (left, top, right, bottom).
fn disp_rect(tf: &PageTransform, r: [f64; 4]) -> (f64, f64, f64, f64) {
    let p = tf.point(r[0] as f32, r[1] as f32);
    let q = tf.point(r[2] as f32, r[3] as f32);
    (p.0.min(q.0) as f64, p.1.min(q.1) as f64, p.0.max(q.0) as f64, p.1.max(q.1) as f64)
}

/// A box in fractions of the display (x, y, w, h) in page space (left, bottom, right, top).
fn display_box_to_page(tf: &PageTransform, b: [f32; 4]) -> [f64; 4] {
    let (dw, dh) = (tf.width(), tf.height());
    let corners = [(b[0], b[1]), (b[0] + b[2], b[1]), (b[0] + b[2], b[1] + b[3]), (b[0], b[1] + b[3])].map(|(x, y)| tf.to_user(x * dw, y * dh));
    let xs = corners.iter().map(|c| c.0 as f64);
    let ys = corners.iter().map(|c| c.1 as f64);
    [xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min), xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max)]
}

/// Share of `b`'s area inside `r` (both left, bottom, right, top).
fn inside_share(b: [f64; 4], r: [f64; 4]) -> f64 {
    let area = ((b[2] - b[0]) * (b[3] - b[1])).max(1e-9);
    let ix = (b[2].min(r[2]) - b[0].max(r[0])).max(0.0);
    let iy = (b[3].min(r[3]) - b[1].max(r[1])).max(0.0);
    if b[2] - b[0] < 1e-6 || b[3] - b[1] < 1e-6 {
        // A degenerate box (a line): inside when its centre is.
        let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        return if cx >= r[0] && cx <= r[2] && cy >= r[1] && cy <= r[3] { 1.0 } else { 0.0 };
    }
    ix * iy / area
}

fn find_in(text: &str, query: &str, opts: FindOptions) -> Vec<(usize, usize)> {
    let fold = |c: char| if opts.match_case { c } else { c.to_lowercase().next().unwrap_or(c) };
    let t: Vec<char> = text.chars().collect();
    let q: Vec<char> = query.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + q.len() <= t.len() {
        if t[i..i + q.len()].iter().zip(&q).all(|(a, b)| fold(*a) == *b) {
            let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
            let ok = !opts.whole_word || (!word(i.checked_sub(1).and_then(|k| t.get(k))) && !word(t.get(i + q.len())));
            if ok {
                out.push((i, i + q.len()));
                i += q.len().max(1);
                continue;
            }
        }
        i += 1;
    }
    out
}

pub(crate) fn load_lopdf(bytes: &[u8], password: Option<&str>) -> anyhow::Result<lopdf::Document> {
    let opts = match password {
        Some(pw) if !pw.is_empty() => lopdf::LoadOptions::with_password(pw),
        _ => lopdf::LoadOptions::default(),
    };
    lopdf::Document::load_mem_with_options(bytes, opts).map_err(|e| anyhow!("this PDF cannot be edited ({e})"))
}

/// Writes `doc` as an incremental update of `prev` (the original file
/// `bytes`): only new and changed objects are appended.
pub(crate) fn save_incremental(bytes: Vec<u8>, prev: lopdf::Document, mut doc: lopdf::Document, output: &Path) -> anyhow::Result<()> {
    let changed: Vec<(ObjectId, Object)> = doc
        .objects
        .iter()
        .filter(|(id, o)| prev.objects.get(id) != Some(o))
        .map(|(id, o)| (*id, o.clone()))
        .collect();
    let max_id = doc.max_id;
    let mut inc = lopdf::IncrementalDocument::create_from(bytes, prev);
    for (id, o) in changed {
        inc.new_document.set_object(id, o);
    }
    inc.new_document.max_id = inc.new_document.max_id.max(max_id);
    if inc.save(output).is_ok() {
        return Ok(());
    }
    // Some files (e.g. still encrypted ones) cannot be updated incrementally.
    doc.save(output).with_context(|| format!("could not write {}", output.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Page analysis
// ---------------------------------------------------------------------------

struct Analysis {
    po: PageObjects,
    /// Runs: indices into `po.objs`.
    runs: Vec<Vec<usize>>,
    texts: Vec<String>,
    geoms: Vec<RunGeom>,
    lines: Vec<Line>,
    paras: Vec<Para>,
    tf: PageTransform,
}

fn analyse(page: &PdfPage) -> anyhow::Result<Analysis> {
    let po = page_objects(page)?;
    let tf = PageTransform::new(page);
    let runs = group_runs(&po.objs);
    let texts: Vec<String> = runs.iter().map(|g| run_text(&po.objs, g)).collect();
    let geoms: Vec<RunGeom> = runs.iter().map(|g| run_geom(&po.objs, g, &tf)).collect();
    let containers: Vec<Vec<usize>> = runs.iter().map(|g| po.objs[g[0]].container().to_vec()).collect();
    let lines = group_lines(&texts, &geoms, &containers);
    let paras = group_paragraphs(&lines, &geoms, &containers, &texts);
    Ok(Analysis { po, runs, texts, geoms, lines, paras, tf })
}

impl Analysis {
    fn run_paths(&self, r: usize) -> Vec<ObjPath> {
        self.runs[r].iter().map(|k| self.po.objs[*k].path.clone()).collect()
    }

    fn run_of(&self, path: &[usize]) -> Option<usize> {
        self.runs.iter().position(|g| g.iter().any(|k| self.po.objs[*k].path == path))
    }

    fn para_of_run(&self, r: usize) -> Option<usize> {
        let line = self.lines.iter().position(|l| l.runs.contains(&r))?;
        self.paras.iter().position(|p| p.lines.contains(&line))
    }

    fn page_text(&self) -> PageText {
        let (pw, ph) = (self.tf.width().max(1.0) as f64, self.tf.height().max(1.0) as f64);
        let mut items = Vec::new();
        let mut item_of_run = vec![None; self.runs.len()];
        for (r, g) in self.runs.iter().enumerate() {
            let text = &self.texts[r];
            if text.trim().is_empty() {
                continue;
            }
            let first = &self.po.objs[g[0]];
            let geo = &self.geoms[r];
            let base = fonts::strip_subset(&first.font).to_string();
            item_of_run[r] = Some(items.len());
            items.push(TextItem {
                objects: self.run_paths(r),
                text: text.clone(),
                x: (geo.left / pw) as f32,
                y: (geo.top / ph) as f32,
                w: ((geo.right - geo.left) / pw) as f32,
                h: ((geo.bottom - geo.top) / ph) as f32,
                font: if base.is_empty() { crate::fonts::analyse_font_name(&first.font).family } else { base },
                size: first.size,
                bold: first.bold_flag || first.fill_stroke,
                italic: first.italic_flag || first.skewed(),
                color: first.color,
                invisible: first.invisible,
                rtl: text.chars().find(|c| c.is_alphabetic()).is_some_and(is_rtl),
                paragraph: self.para_of_run(r).unwrap_or(0),
                baseline: (geo.baseline / ph) as f32,
                outlined: false,
            });
        }
        let paragraphs = self
            .paras
            .iter()
            .map(|p| ParagraphInfo {
                items: p.lines.iter().flat_map(|l| self.lines[*l].runs.iter().filter_map(|r| item_of_run[*r])).collect(),
                text: p.text.clone(),
                x: (p.left / pw) as f32,
                y: (p.top / ph) as f32,
                w: ((p.right - p.left) / pw) as f32,
                h: ((p.bottom - p.top) / ph) as f32,
                align: p.align,
                lines: p.lines.len(),
            })
            .collect();
        let images = self
            .po
            .images
            .iter()
            .map(|im| {
                let r = disp_rect(&self.tf, im.rect);
                ImageItem { path: im.path.clone(), x: (r.0 / pw) as f32, y: (r.1 / ph) as f32, w: ((r.2 - r.0) / pw) as f32, h: ((r.3 - r.1) / ph) as f32 }
            })
            .collect();
        PageText { width: pw as f32, height: ph as f32, items, paragraphs, outlined_shapes: self.outline_shapes().len(), images, tables: Vec::new() }
    }

    /// Letter-like outline shapes away from real text (display points: left, top, right, bottom).
    fn outline_shapes(&self) -> Vec<(f64, f64, f64, f64)> {
        self.po
            .glyph_paths
            .iter()
            .map(|r| {
                let p = self.tf.point(r[0] as f32, r[1] as f32);
                let q = self.tf.point(r[2] as f32, r[3] as f32);
                (p.0.min(q.0) as f64, p.1.min(q.1) as f64, p.0.max(q.0) as f64, p.1.max(q.1) as f64)
            })
            .filter(|s| !self.geoms.iter().any(|g| s.0 < g.right && s.2 > g.left && s.1 < g.bottom && s.3 > g.top))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Planned output
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum RunFont {
    /// A font resource of the content stream (name as written).
    Res(Vec<u8>),
    /// An installed font (index into the session's fallbacks).
    Fallback(usize),
    /// A font object (added to the resources when needed).
    Obj(ObjectId),
}

#[derive(Clone, Debug)]
enum Item {
    Tok(Tok),
    Piece(Piece),
}

#[derive(Clone, Debug, Default)]
struct SegRun {
    /// `None`: the font of the operator being replaced.
    font: Option<RunFont>,
    size: f64,
    color: Option<[u8; 3]>,
    /// Synthetic bold: stroke width (user space).
    stroke: Option<f64>,
    /// Text render mode to set (e.g. 0 to undo a synthetic bold).
    render: Option<i32>,
    /// Horizontal scaling (fraction).
    tz: Option<f64>,
    /// Character and word spacing.
    tc: Option<f64>,
    tw: Option<f64>,
    /// Text matrix for this run (else it follows the previous run).
    tm: Option<Mat>,
    items: Vec<Item>,
    base_rise: f64,
}

/// How a paragraph is laid out again.
#[derive(Clone, Copy, Debug, Default)]
struct Wrap {
    /// Make the text smaller (down to 70 %, or 60 % in a frame) rather than use more lines.
    fit: bool,
    /// Move the text below down when the paragraph grows.
    push_down: bool,
    /// Move what no longer fits in the column on to the next column and page.
    flow: bool,
    /// A box to wrap in (display points: left, top, right, bottom), such as a table cell.
    frame: Option<[f64; 4]>,
}

/// A paragraph laid out again.
struct Wrapped {
    c: usize,
    shows: Vec<usize>,
    anchor: Show,
    /// The anchor's text space → container space.
    ma: Mat,
    /// Container space → page space.
    to_page: Mat,
    /// The runs of each new line (text matrices in the anchor's text space).
    lines: Vec<Vec<SegRun>>,
    /// Baseline step (anchor text space).
    pitch: f64,
    old_lines: usize,
    /// The paragraph's runs.
    runs: Vec<usize>,
    /// More lines than the frame holds.
    overfull: bool,
}

#[derive(Clone, Debug)]
struct Segment {
    /// Operators kept from a `"` operator (state it sets).
    prefix: String,
    /// Reset character spacing, word spacing, scaling and rise first.
    reset: bool,
    runs: Vec<SegRun>,
    /// Back to the text position the replaced operator left (see [`resume`]).
    resume: String,
}

#[derive(Clone, Debug)]
enum Part {
    Raw(String),
    Tf(RunFont, f64),
    Tj(Vec<Item>, f64, f64),
}

#[derive(Clone, Debug)]
enum Out {
    Parts(Vec<Part>),
    /// Drawn in its own text object where the replaced operator was.
    Seg(Segment),
    /// Drawn at the end of the content, in the page's own coordinates.
    Block(Segment),
}

/// One content stream being edited: the page's, or a form XObject's.
struct CPlan {
    path: ObjPath,
    content: Vec<u8>,
    scan: Scan,
    scan_error: Option<String>,
    fonts: HashMap<Vec<u8>, FontMeta>,
    /// The form XObject (for forms) and its rank among its parent's forms.
    xobject: Option<ObjectId>,
    rank: usize,
    /// The form's matrix: the coordinate space its content starts in.
    matrix: Mat,
    resources: Dictionary,
    res_dirty: bool,
    splices: Vec<(usize, usize, Out)>,
    appended: Vec<Out>,
}

impl CPlan {
    fn show_of(&self, index: usize) -> Option<usize> {
        self.scan.shows.iter().position(|s| s.object == index)
    }

    fn check(&self) -> anyhow::Result<()> {
        match &self.scan_error {
            Some(e) => bail!("the text cannot be edited here: {e}"),
            None => Ok(()),
        }
    }

    /// Resource name for object `id` in `category` ("Font", "XObject").
    fn resource_name(&mut self, doc: &lopdf::Document, category: &[u8], id: ObjectId) -> Vec<u8> {
        let mut sub = self.resources.get(category).ok().map(|o| deref(doc, o)).and_then(|o| o.as_dict().ok()).cloned().unwrap_or_default();
        if let Some((name, _)) = sub.iter().find(|(_, v)| matches!(v, Object::Reference(r) if *r == id)) {
            return name.clone();
        }
        let prefix = if category == b"Font" { "P2WF" } else { "P2WX" };
        let name = (1..).map(|i| format!("{prefix}{i}").into_bytes()).find(|n| !sub.has(n)).unwrap_or_default();
        sub.set(name.clone(), id);
        self.resources.set(category.to_vec(), sub);
        self.res_dirty = true;
        name
    }
}

struct PagePlan {
    page: usize,
    page_id: ObjectId,
    containers: Vec<CPlan>,
}

struct Session<'a, 'b> {
    pdoc: &'a PdfDocument<'b>,
    doc: lopdf::Document,
    page_ids: Vec<ObjectId>,
    ctx: Ctx,
    plans: Vec<PagePlan>,
    /// Content streams built now are learned from.
    learning: bool,
    /// Lines that flow on to the next page.
    carry: Option<flow::Carry>,
}

/// The page's resource dictionary (inherited ones included).
fn page_resources(doc: &lopdf::Document, page_id: ObjectId) -> Dictionary {
    let mut id = page_id;
    for _ in 0..64 {
        let Ok(d) = doc.get_dictionary(id) else { break };
        if let Ok(r) = d.get(b"Resources") {
            return deref(doc, r).as_dict().cloned().unwrap_or_default();
        }
        match d.get(b"Parent").and_then(|o| o.as_reference()) {
            Ok(p) => id = p,
            Err(_) => break,
        }
    }
    Dictionary::new()
}

/// Names of the XObjects of a subtype ("Form", "Image") in a resource dictionary.
fn xobject_names(doc: &lopdf::Document, res: &Dictionary, subtype: &[u8]) -> HashSet<Vec<u8>> {
    let mut out = HashSet::new();
    if let Ok(x) = res.get(b"XObject").map(|o| deref(doc, o)).and_then(|o| o.as_dict()) {
        for (name, v) in x.iter() {
            if let Ok(s) = deref(doc, v).as_stream() {
                if s.dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()) == Some(subtype) {
                    out.insert(name.clone());
                }
            }
        }
    }
    out
}

impl<'a, 'b> Session<'a, 'b> {
    fn new(pdoc: &'a PdfDocument<'b>, doc: lopdf::Document) -> anyhow::Result<Self> {
        let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
        if page_ids.len() != pdoc.pages().len() as usize {
            bail!("this PDF cannot be edited (its page tree is damaged)");
        }
        Ok(Session { pdoc, doc, page_ids, ctx: Ctx::new(), plans: Vec::new(), learning: false, carry: None })
    }

    /// Builds (or finds) the plan for content stream `path` of a page.
    fn container(&mut self, plan: &mut PagePlan, po: &PageObjects, path: &[usize]) -> anyhow::Result<usize> {
        if let Some(i) = plan.containers.iter().position(|c| c.path == path) {
            return Ok(i);
        }
        let mut form_rank = 0;
        let (content, resources, xobject, matrix) = if path.is_empty() {
            (self.doc.get_page_content(plan.page_id), page_resources(&self.doc, plan.page_id), None, IDENTITY)
        } else {
            let parent_path = &path[..path.len() - 1];
            let parent = self.container(plan, po, parent_path)?;
            let info = po.container(parent_path).ok_or_else(|| anyhow!("unknown container"))?;
            let rank = info.forms.iter().position(|i| Some(i) == path.last()).ok_or_else(|| anyhow!("unknown form object"))?;
            form_rank = rank;
            let p = &plan.containers[parent];
            p.check()?;
            let op = p.scan.form_dos.get(rank).map(|k| &p.scan.ops[*k]).ok_or_else(|| anyhow!("could not find the form XObject in the content"))?;
            let Some(content::Arg::Name(raw)) = op.args.first() else { bail!("bad Do operator") };
            let name = content::decode_name(raw);
            let xid = p
                .resources
                .get(b"XObject")
                .ok()
                .map(|o| deref(&self.doc, o))
                .and_then(|o| o.as_dict().ok())
                .and_then(|d| d.get(&name).ok())
                .and_then(|o| o.as_reference().ok())
                .ok_or_else(|| anyhow!("the form XObject is not an indirect object"))?;
            let stream = self.doc.get_object(xid)?.as_stream()?.clone();
            let content = stream.decompressed_content().unwrap_or_else(|_| stream.content.clone());
            let matrix = stream
                .dict
                .get(b"Matrix")
                .ok()
                .and_then(|o| deref(&self.doc, o).as_array().ok())
                .filter(|a| a.len() == 6)
                .map(|a| {
                    let v: Vec<f64> = a.iter().map(|o| fonts::obj_num(&self.doc, o).unwrap_or(0.0)).collect();
                    [v[0], v[1], v[2], v[3], v[4], v[5]]
                })
                .unwrap_or(IDENTITY);
            let resources = match stream.dict.get(b"Resources") {
                Ok(r) => deref(&self.doc, r).as_dict().cloned().unwrap_or_default(),
                Err(_) => p.resources.clone(),
            };
            (content, resources, Some(xid), matrix)
        };
        let owner = xobject.unwrap_or(plan.page_id);
        let fonts = resource_fonts(&self.doc, &resources, owner);
        let forms = xobject_names(&self.doc, &resources, b"Form");
        let images = xobject_names(&self.doc, &resources, b"Image");
        let objs = po.text_in(path);
        let (scan, scan_error) = match content::scan(&content, &objs, &forms, &images, matrix) {
            Ok(s) => (s, None),
            Err(e) => (Scan::ops_only(&content, &forms, &images), Some(e.to_string())),
        };
        if scan_error.is_none() && self.learning {
            let by_index: HashMap<usize, &PObj> = po.objs.iter().filter(|o| o.container() == path).map(|o| (o.index(), o)).collect();
            self.ctx.learn(&scan, &by_index, &fonts);
        }
        plan.containers.push(CPlan {
            path: path.to_vec(),
            content,
            scan,
            scan_error,
            fonts,
            xobject,
            rank: form_rank,
            matrix,
            resources,
            res_dirty: false,
            splices: Vec::new(),
            appended: Vec::new(),
        });
        Ok(plan.containers.len() - 1)
    }

    /// Learns the glyphs of every font on a page.
    fn learn_page(&mut self, page: usize) {
        if !self.ctx.learned_pages.insert(page) {
            return;
        }
        let Ok(p) = self.pdoc.pages().get(page as PdfPageIndex) else { return };
        let Ok(po) = page_objects(&p) else { return };
        let mut plan = PagePlan { page, page_id: self.page_ids[page], containers: Vec::new() };
        let paths: Vec<ObjPath> = po.containers.iter().map(|c| c.path.clone()).collect();
        self.learning = true;
        for path in paths {
            let _ = self.container(&mut plan, &po, &path);
        }
        self.learning = false;
    }

    /// Learns from other pages until `meta`'s font is known to have all of `text`.
    fn learn_for(&mut self, meta_key: &fonts::FontKey, text: &str) {
        let chars: HashSet<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
        for page in 0..self.page_ids.len() {
            let missing = chars.iter().any(|c| !self.ctx.learned.get(meta_key).is_some_and(|l| l.codes.contains_key(c)));
            if !missing {
                break;
            }
            self.learn_page(page);
        }
    }

    fn edit_page(&mut self, page: usize, edits: &[&TextEdit]) -> anyhow::Result<()> {
        let ppage = self.pdoc.pages().get(page as PdfPageIndex)?;
        let a = analyse(&ppage)?;
        self.learn_page(page);
        let mut plan = PagePlan { page, page_id: self.page_ids[page], containers: Vec::new() };
        let mut touched: HashSet<ObjPath> = HashSet::new();
        // Text flowing on from the page before (placed after this page's own edits).
        let incoming = self.carry.take();
        for e in edits {
            let objects: &[ObjPath] = match e {
                TextEdit::Replace { objects, .. }
                | TextEdit::Delete { objects, .. }
                | TextEdit::Restyle { objects, .. }
                | TextEdit::Move { objects, .. }
                | TextEdit::Paragraph { objects, .. }
                | TextEdit::Cell { objects, .. } => objects,
                TextEdit::Insert { .. } | TextEdit::Outlined { .. } | TextEdit::Redact { .. } | TextEdit::Image { .. } | TextEdit::InsertImage { .. } => &[],
            };
            for o in objects {
                if !touched.insert(o.clone()) {
                    bail!("text object {o:?} is edited twice");
                }
            }
            match e {
                TextEdit::Replace { objects, text, fit, .. } => {
                    if text.trim().is_empty() {
                        self.delete(&mut plan, &a, objects)?;
                    } else {
                        self.replace(&mut plan, &a, objects, text, *fit)?;
                    }
                }
                TextEdit::Delete { objects, .. } => self.delete(&mut plan, &a, objects)?,
                TextEdit::Insert { x, y, text, size, color, style, .. } => self.insert(&mut plan, &a, *x, *y, text, *size, *color, style)?,
                TextEdit::Restyle { objects, text, change, fit, .. } => self.restyle(&mut plan, &a, objects, text.as_deref(), change, *fit)?,
                TextEdit::Move { objects, dx, dy, .. } => {
                    let v = display_to_page_vec(&a.tf, *dx as f64 * a.tf.width() as f64, *dy as f64 * a.tf.height() as f64);
                    self.move_objects(&mut plan, &a, objects, v)?
                }
                TextEdit::Paragraph { objects, text, fit, push_down, flow, .. } => {
                    self.paragraph(&mut plan, &a, objects, text, Wrap { fit: *fit, push_down: *push_down, flow: *flow, frame: None }, &mut touched)?
                }
                TextEdit::Cell { x, y, w, h, objects, text, fit, .. } => {
                    let (dw, dh) = (a.tf.width() as f64, a.tf.height() as f64);
                    let cell = [*x as f64 * dw, *y as f64 * dh, (*x + *w) as f64 * dw, (*y + *h) as f64 * dh];
                    self.cell(&mut plan, &a, cell, objects, text, *fit, &mut touched)?
                }
                TextEdit::Redact { x, y, w, h, fill, .. } => {
                    let region = display_box_to_page(&a.tf, [*x, *y, *w, *h]);
                    self.redact(&mut plan, &a, region, fill.unwrap_or([0, 0, 0]))?
                }
                TextEdit::Image { path, change, .. } => self.image(&mut plan, &a, path, change)?,
                TextEdit::InsertImage { x, y, w, h, file, .. } => self.insert_image(&mut plan, &a, [*x, *y, *w, *h], file)?,
                TextEdit::Outlined { x, y, w, h, baseline, text, size, color, style, fit, .. } => {
                    self.outlined(&mut plan, &a, [*x, *y, *w, *h], *baseline, text, *size, *color, style, *fit)?
                }
            }
        }
        if let Some(carry) = incoming {
            self.flow_in(&mut plan, &a, carry, &mut touched)?;
        }
        self.plans.push(plan);
        Ok(())
    }

    /// The content stream and operators of a run's objects (in content order).
    fn shows_of(&mut self, plan: &mut PagePlan, a: &Analysis, objects: &[ObjPath]) -> anyhow::Result<(usize, Vec<usize>)> {
        let first = objects.first().ok_or_else(|| anyhow!("no text objects given"))?;
        if first.is_empty() {
            bail!("bad object path");
        }
        let container_path = first[..first.len() - 1].to_vec();
        if objects.iter().any(|o| o.len() != first.len() || o[..o.len() - 1] != container_path[..]) {
            bail!("the text objects are in different content streams");
        }
        let c = self.container(plan, &a.po, &container_path)?;
        plan.containers[c].check()?;
        let mut shows = Vec::new();
        for o in objects {
            let idx = *o.last().unwrap();
            let k = plan.containers[c].show_of(idx).ok_or_else(|| anyhow!("text object {o:?} is not part of the content"))?;
            shows.push(k);
        }
        shows.sort_by_key(|k| plan.containers[c].scan.shows[*k].op);
        Ok((c, shows))
    }

    fn delete(&mut self, plan: &mut PagePlan, a: &Analysis, objects: &[ObjPath]) -> anyhow::Result<()> {
        let (c, shows) = self.shows_of(plan, a, objects)?;
        let cp = &mut plan.containers[c];
        for k in shows {
            let sh = &cp.scan.shows[k];
            let op = &cp.scan.ops[sh.op];
            let text = neutralize(sh)?;
            cp.splices.push((op.start, op.end, Out::Parts(vec![Part::Raw(text)])));
        }
        Ok(())
    }

    /// Pieces of `text` in a run's own font, or an installed font.
    fn pieces_for(&mut self, page: usize, plan: &PagePlan, c: usize, sh: &Show, o: &PObj, text: &str) -> anyhow::Result<(Option<usize>, Vec<Piece>)> {
        let cp = &plan.containers[c];
        let meta = cp.fonts.get(&content::decode_name(&sh.font));
        if let Some(meta) = meta {
            if meta.embedded {
                let key = meta.key.clone();
                self.learn_for(&key, text);
            }
            let meta = plan.containers[c].fonts.get(&content::decode_name(&sh.font)).unwrap();
            if let Some(p) = encode_in_font(&mut self.ctx, &self.doc, meta, text) {
                return Ok((None, p));
            }
            let (fb, p) = fallback_text(&mut self.ctx, page, Some(()), &meta.family, &meta.base, meta.bold, meta.italic, o.serif, o.mono, text)?;
            return Ok((Some(fb), p));
        }
        let (fb, p) = fallback_text(&mut self.ctx, page, Some(()), "Arial", "", o.bold_flag, o.italic_flag, o.serif, o.mono, text)?;
        Ok((Some(fb), p))
    }

    fn replace(&mut self, plan: &mut PagePlan, a: &Analysis, objects: &[ObjPath], text: &str, fit: bool) -> anyhow::Result<()> {
        let (c, shows) = self.shows_of(plan, a, objects)?;
        let page = plan.page;
        let first = plan.containers[c].scan.shows[shows[0]].clone();
        let pobjs: Vec<&PObj> = shows.iter().filter_map(|k| a.po.find(&objects_path(&plan.containers[c], *k))).collect();
        let o = *pobjs.first().ok_or_else(|| anyhow!("text object not found"))?;
        let old_text: String = pobjs.iter().map(|o| o.text()).collect();
        let complex = |s: &str| s.chars().any(|c| is_rtl(c) || needs_shaping(c));

        // One operator: keep the unchanged start and end exactly as they were.
        if shows.len() == 1 && !complex(text) && !complex(&old_text) {
            if let Some(done) = self.replace_middle(plan, c, &first, o, text, fit)? {
                let cp = &mut plan.containers[c];
                let op = &cp.scan.ops[first.op];
                cp.splices.push((op.start, op.end, done));
                return Ok(());
            }
        }

        // Several operators, or right-to-left / complex text: the run is drawn again.
        let (fb, pieces) = self.pieces_for(page, plan, c, &first, o, text)?;
        let tw = if fb.is_some() { 0.0 } else { first.tw };
        let new_adv = pieces_advance(&pieces, first.size, first.tc, tw, first.th);
        let m = mul(&first.tm(), &first.ctm);
        let ext = pobjs.iter().map(|oo| objects::extent(o, oo)).fold(0.0f64, f64::max);
        let old_adv = ext / x_scale(&m).max(1e-9);
        let th = fit_scale(fit, first.th, old_adv, new_adv);
        let shown = new_adv * th.map(|t| t / first.th).unwrap_or(1.0);
        // Right-to-left text keeps its right end where it was.
        let shift = anchor_shift(a, a.run_of(&objects[0]), text, old_adv, shown);
        let tm = mul(&translate(shift, 0.0), &first.tm());
        let run = SegRun {
            font: fb.map(RunFont::Fallback),
            size: first.size,
            tz: th,
            tm: Some(tm),
            items: pieces.into_iter().map(Item::Piece).collect(),
            base_rise: first.rise,
            ..Default::default()
        };
        self.place_segment(plan, c, &shows, Segment { prefix: quote_prefix(&first, false), reset: false, runs: vec![run], resume: resume(&first) })
    }

    /// Replaces only the changed middle of one operator's text.
    fn replace_middle(&mut self, plan: &mut PagePlan, c: usize, first: &Show, o: &PObj, text: &str, fit: bool) -> anyhow::Result<Option<Out>> {
        let page = plan.page;
        let Some(meta) = plan.containers[c].fonts.get(&content::decode_name(&first.font)) else { return Ok(None) };
        let Some(len) = meta.code_len else { return Ok(None) };
        let mut toks = Vec::new();
        for p in &first.parts {
            match p {
                content::Arg::Num(n) => toks.push(Tok::Kern(*n)),
                content::Arg::Str(b) => toks.extend(split_codes(b, len).into_iter().map(Tok::Code)),
                _ => {}
            }
        }
        let n_codes = toks.iter().filter(|t| matches!(t, Tok::Code(_))).count();
        let real: Vec<char> = o.seq.iter().filter(|(_, g)| !g).map(|(c, _)| *c).collect();
        if n_codes != real.len() {
            return Ok(None);
        }
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
            return Ok(Some(Out::Parts(vec![Part::Raw(op_text(&plan.containers[c], first))])));
        }
        let codes_before = |n: usize| o.seq[..n].iter().filter(|(_, g)| !g).count();
        let cp_ = codes_before(p);
        let cs = n_codes - codes_before(old.len() - sfx);
        let code_pos: Vec<usize> = toks.iter().enumerate().filter(|(_, t)| matches!(t, Tok::Code(_))).map(|(i, _)| i).collect();
        let mut pre_end = if cp_ == 0 { code_pos.first().copied().unwrap_or(toks.len()) } else { code_pos[cp_ - 1] + 1 };
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
        let (prefix, suffix) = (toks[..pre_end].to_vec(), toks[suf_start..].to_vec());
        let width_of = |ctx: &Ctx, toks: &[Tok], chars: &[char]| tokens_advance(ctx, meta, first, toks, chars);
        let old_adv = width_of(&self.ctx, &toks, &real);
        if meta.embedded {
            let key = meta.key.clone();
            self.learn_for(&key, &middle);
        }
        let meta = plan.containers[c].fonts.get(&content::decode_name(&first.font)).unwrap();
        let mut parts = vec![Part::Raw(quote_prefix(first, true))];
        let (pieces, fb) = match encode_in_font(&mut self.ctx, &self.doc, meta, &middle) {
            Some(p) => (p, None),
            None => {
                let (fb, p) = fallback_text(&mut self.ctx, page, Some(()), &meta.family, &meta.base, meta.bold, meta.italic, o.serif, o.mono, &middle)?;
                (p, Some(fb))
            }
        };
        let new_adv = width_of(&self.ctx, &prefix, &real[..cp_])
            + pieces_advance(&pieces, first.size, first.tc, if fb.is_some() { 0.0 } else { first.tw }, first.th)
            + width_of(&self.ctx, &suffix, &real[real.len() - cs..]);
        let th = fit_scale(fit, first.th, old_adv, new_adv);
        if let Some(th) = th {
            parts.push(Part::Raw(format!("{} Tz ", fmt(th * 100.0))));
        }
        let items = |toks: Vec<Tok>| toks.into_iter().map(Item::Tok).collect::<Vec<_>>();
        match fb {
            None => {
                let mut all = items(prefix);
                all.extend(pieces.into_iter().map(Item::Piece));
                all.extend(items(suffix));
                parts.push(Part::Tj(all, first.size, first.rise));
            }
            Some(fb) => {
                parts.push(Part::Tj(items(prefix), first.size, first.rise));
                parts.push(Part::Tf(RunFont::Fallback(fb), first.size));
                parts.push(Part::Tj(pieces.into_iter().map(Item::Piece).collect(), first.size, first.rise));
                parts.push(Part::Tf(RunFont::Res(first.font.clone()), first.size));
                parts.push(Part::Tj(items(suffix), first.size, first.rise));
            }
        }
        if th.is_some() {
            parts.push(Part::Raw(format!(" {} Tz", fmt(first.th * 100.0))));
        }
        Ok(Some(Out::Parts(parts)))
    }

    /// Draws `seg` where the first operator was and removes the others.
    fn place_segment(&mut self, plan: &mut PagePlan, c: usize, shows: &[usize], seg: Segment) -> anyhow::Result<()> {
        let cp = &mut plan.containers[c];
        let first = &cp.scan.shows[shows[0]];
        let op = &cp.scan.ops[first.op];
        cp.splices.push((op.start, op.end, Out::Seg(seg)));
        for &k in &shows[1..] {
            let sh = &cp.scan.shows[k];
            let op = &cp.scan.ops[sh.op];
            let text = neutralize(sh)?;
            cp.splices.push((op.start, op.end, Out::Parts(vec![Part::Raw(text)])));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert(&mut self, plan: &mut PagePlan, a: &Analysis, x: f32, y: f32, text: &str, size: f32, color: [u8; 3], style: &NewTextStyle) -> anyhow::Result<()> {
        let (dw, dh) = (a.tf.width() as f64, a.tf.height() as f64);
        let size = size as f64;
        self.draw_text(plan, a, x as f64 * dw, y as f64 * dh + 0.8 * size, text, size, color, style, None)
    }

    /// New text on the page (display points), drawn after everything else.
    #[allow(clippy::too_many_arguments)]
    fn draw_text(&mut self, plan: &mut PagePlan, a: &Analysis, px: f64, baseline: f64, text: &str, size: f64, color: [u8; 3], style: &NewTextStyle, max_width: Option<f64>) -> anyhow::Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let family = match style.family.to_ascii_lowercase().as_str() {
            "" | "sans" | "sans-serif" => "Arial".to_string(),
            "serif" => "Times New Roman".to_string(),
            "mono" | "monospace" => "Courier New".to_string(),
            _ => style.family.clone(),
        };
        let (fb, pieces) = fallback_text(&mut self.ctx, plan.page, None, &family, "", style.bold, style.italic, family == "Times New Roman", family == "Courier New", text)?;
        let used = self.ctx.fallbacks[fb].family.clone();
        let generic = matches!(style.family.to_ascii_lowercase().as_str(), "" | "sans" | "sans-serif" | "serif" | "mono" | "monospace");
        if !generic && !used.eq_ignore_ascii_case(&family) {
            self.ctx.note(format!("Page {}: {family} is not installed; the new text uses {used}.", plan.page + 1));
        }
        let tf = &a.tf;
        let o = tf.to_user(px as f32, baseline as f32);
        let r = tf.to_user(px as f32 + 1.0, baseline as f32);
        let u = tf.to_user(px as f32, baseline as f32 - 1.0);
        let m: Mat = [(r.0 - o.0) as f64, (r.1 - o.1) as f64, (u.0 - o.0) as f64, (u.1 - o.1) as f64, o.0 as f64, o.1 as f64];
        let width = pieces_advance(&pieces, size, 0.0, 0.0, 1.0);
        let tz = max_width.filter(|w| *w > 0.0 && width > *w * 1.01).map(|w| (w / width).max(0.5));
        let c = self.container(plan, &a.po, &[])?;
        let run = SegRun { font: Some(RunFont::Fallback(fb)), size, color: Some(color), tz, tm: Some(m), items: pieces.into_iter().map(Item::Piece).collect(), ..Default::default() };
        plan.containers[c].appended.push(Out::Block(Segment { prefix: String::new(), reset: false, runs: vec![run], resume: String::new() }));
        Ok(())
    }

    /// Permanently removes what lies in `region` (page space) and covers it.
    fn redact(&mut self, plan: &mut PagePlan, a: &Analysis, region: [f64; 4], fill: [u8; 3]) -> anyhow::Result<()> {
        let containers: Vec<(ObjPath, Mat, Vec<usize>)> = a.po.containers.iter().map(|c| (c.path.clone(), c.to_page, c.images.clone())).collect();
        let mut partial = false;
        for (path, to_page, image_objs) in containers {
            let c = self.container(plan, &a.po, &path)?;
            // Text: the glyphs inside are replaced by their advance.
            let shows = plan.containers[c].scan.shows.clone();
            let has_text_inside = a.po.objs.iter().any(|o| o.container() == path && o.boxes.iter().zip(&o.seq).any(|(b, (_, g))| !g && inside_share(*b, region) > 0.3));
            if has_text_inside {
                plan.containers[c].check().context("the text here cannot be redacted safely")?;
            }
            for sh in shows {
                let mut opath = path.clone();
                opath.push(sh.object);
                let Some(o) = a.po.find(&opath) else { continue };
                let hits: Vec<usize> = o.seq.iter().enumerate().filter(|(i, (_, g))| !g && inside_share(o.boxes[*i], region) > 0.3).map(|(i, _)| i).collect();
                if hits.is_empty() {
                    continue;
                }
                let cp = &plan.containers[c];
                let op = &cp.scan.ops[sh.op];
                let (s0, e0) = (op.start, op.end);
                let replacement = match cp.fonts.get(&content::decode_name(&sh.font)).and_then(|m| m.code_len.map(|l| (m, l))) {
                    Some((meta, len)) => {
                        let real_index: Vec<usize> = o.seq.iter().enumerate().filter(|(_, (_, g))| !g).map(|(i, _)| i).collect();
                        let n_codes: usize = sh.parts.iter().map(|p| if let content::Arg::Str(b) = p { b.len() / len } else { 0 }).sum();
                        if n_codes == real_index.len() {
                            let mut toks = Vec::new();
                            let mut k = 0;
                            for p in &sh.parts {
                                match p {
                                    content::Arg::Num(n) => toks.push(Tok::Kern(*n)),
                                    content::Arg::Str(b) => {
                                        for code in split_codes(b, len) {
                                            let ci = real_index[k];
                                            k += 1;
                                            if hits.contains(&ci) {
                                                let em = if meta.has_widths {
                                                    *meta.widths.get(&fonts::code_value(&code)).unwrap_or(&meta.missing)
                                                } else {
                                                    let bx = o.boxes[ci];
                                                    (bx[2] - bx[0]) / (o.size as f64).max(0.1)
                                                };
                                                let ws = if code == [32] && len == 1 { sh.tw } else { 0.0 };
                                                let size = sh.size.abs().max(1e-6);
                                                toks.push(Tok::Kern(-(em * 1000.0 + (sh.tc + ws) * 1000.0 / size)));
                                            } else {
                                                toks.push(Tok::Code(code));
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            format!("{}{}", quote_prefix(&sh, true), tj_ops(&toks, sh.size, sh.rise))
                        } else {
                            partial = true;
                            neutralize(&sh)?
                        }
                    }
                    None => {
                        partial = true;
                        neutralize(&sh)?
                    }
                };
                plan.containers[c].splices.push((s0, e0, Out::Parts(vec![Part::Raw(replacement)])));
            }
            // Shapes mostly inside.
            let cp = &mut plan.containers[c];
            for paint in content::scan_paths(&cp.scan.ops, cp.matrix) {
                if paint.clip {
                    continue;
                }
                let hits: Vec<bool> = paint.subpaths.iter().map(|sp| inside_share(objects::transform_rect(&to_page, sp.2), region) > 0.5).collect();
                for (sp, hit) in paint.subpaths.iter().zip(&hits) {
                    if *hit {
                        cp.splices.push((sp.0, sp.1, Out::Parts(vec![Part::Raw(String::new())])));
                    }
                }
                if !hits.is_empty() && hits.iter().all(|h| *h) {
                    cp.splices.push((paint.paint.0, paint.paint.1, Out::Parts(vec![Part::Raw(String::new())])));
                }
            }
            // Pictures: the pixels inside are painted over.
            for (k, idx) in image_objs.iter().enumerate() {
                let mut ipath = path.clone();
                ipath.push(*idx);
                let Some(img) = a.po.images.iter().find(|i| i.path == ipath) else { continue };
                if inside_share(img.rect, region) <= 0.0 {
                    continue;
                }
                let cp = &plan.containers[c];
                let op_index = *cp.scan.image_ops.get(k).ok_or_else(|| anyhow!("a picture here cannot be redacted safely"))?;
                let op = cp.scan.ops[op_index].clone();
                if op.name == b"BI" {
                    // Inline image: removed entirely.
                    plan.containers[c].splices.push((op.start, op.end, Out::Parts(vec![Part::Raw(String::new())])));
                    partial = true;
                    continue;
                }
                let Some(content::Arg::Name(raw)) = op.args.first() else { continue };
                let name = content::decode_name(raw);
                let xid = plan.containers[c]
                    .resources
                    .get(b"XObject")
                    .ok()
                    .map(|o| deref(&self.doc, o))
                    .and_then(|o| o.as_dict().ok())
                    .and_then(|d| d.get(&name).ok())
                    .and_then(|o| o.as_reference().ok())
                    .ok_or_else(|| anyhow!("a picture here cannot be redacted safely"))?;
                let new_id = self.redact_image(plan.page, &ipath, xid, &mul(&img.matrix, &img.to_page), region, fill)?;
                let cp = &mut plan.containers[c];
                let new_name = cp.resource_name(&self.doc, b"XObject", new_id);
                cp.splices.push((op.start, op.end, Out::Parts(vec![Part::Raw(format!("{} Do", write_name(&new_name)))])));
            }
        }
        // Annotations over the area.
        let page_id = plan.page_id;
        let annots = self.doc.get_dictionary(page_id)?.get(b"Annots").ok().map(|o| deref(&self.doc, o).clone());
        if let Some(Object::Array(list)) = annots {
            let keep: Vec<Object> = list
                .iter()
                .filter(|o| {
                    let r = deref(&self.doc, o).as_dict().ok().and_then(|d| d.get(b"Rect").ok()).and_then(|r| deref(&self.doc, r).as_array().ok()).map(|a| {
                        let v: Vec<f64> = a.iter().map(|x| fonts::obj_num(&self.doc, x).unwrap_or(0.0)).collect();
                        [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])]
                    });
                    !r.is_some_and(|r| r[0] < region[2] && r[2] > region[0] && r[1] < region[3] && r[3] > region[1])
                })
                .cloned()
                .collect();
            if keep.len() != list.len() {
                self.doc.get_dictionary_mut(page_id)?.set("Annots", Object::Array(keep));
            }
        }
        if partial {
            self.ctx.note(format!("Page {}: some text or pictures touching the redacted area were removed entirely.", plan.page + 1));
        }
        // The box itself.
        let c = self.container(plan, &a.po, &[])?;
        let [r, g, b] = fill;
        let cover = format!(
            "q {} {} {} rg {} {} {} {} re f Q\n",
            fmt(r as f64 / 255.0),
            fmt(g as f64 / 255.0),
            fmt(b as f64 / 255.0),
            fmt(region[0]),
            fmt(region[1]),
            fmt(region[2] - region[0]),
            fmt(region[3] - region[1])
        );
        plan.containers[c].appended.push(Out::Parts(vec![Part::Raw(cover)]));
        Ok(())
    }

    /// The PDFium image object at `path` on a page, decoded.
    fn decoded_image(&self, page: usize, path: &[usize]) -> anyhow::Result<image::DynamicImage> {
        fn find(obj: &PdfPageObject, rest: &[usize]) -> anyhow::Result<image::DynamicImage> {
            match rest.split_first() {
                None => Ok(obj.as_image_object().ok_or_else(|| anyhow!("not a picture"))?.get_raw_image()?),
                Some((k, rest)) => {
                    let child = obj.as_x_object_form_object().ok_or_else(|| anyhow!("picture not found"))?.get(*k as PdfPageObjectIndex)?;
                    find(&child, rest)
                }
            }
        }
        let p = self.pdoc.pages().get(page as PdfPageIndex)?;
        let obj = p.objects().get(path[0] as PdfPageObjectIndex)?;
        find(&obj, &path[1..])
    }

    /// A copy of image `xid` with the pixels inside `region` painted over.
    fn redact_image(&mut self, page: usize, path: &[usize], xid: ObjectId, unit_to_page: &Mat, region: [f64; 4], fill: [u8; 3]) -> anyhow::Result<ObjectId> {
        let decoded = self.decoded_image(page, path)?;
        let mut rgb = decoded.to_rgb8();
        let (w, h) = (rgb.width() as f64, rgb.height() as f64);
        let inv = invert(unit_to_page).ok_or_else(|| anyhow!("degenerate picture"))?;
        let corners = [(region[0], region[1]), (region[2], region[1]), (region[2], region[3]), (region[0], region[3])].map(|(x, y)| apply(&inv, x, y));
        let (u0, u1) = (corners.iter().map(|c| c.0).fold(f64::MAX, f64::min), corners.iter().map(|c| c.0).fold(f64::MIN, f64::max));
        let (v0, v1) = (corners.iter().map(|c| c.1).fold(f64::MAX, f64::min), corners.iter().map(|c| c.1).fold(f64::MIN, f64::max));
        let x0 = (u0.max(0.0) * w).floor() as u32;
        let x1 = ((u1.min(1.0) * w).ceil() as u32).min(rgb.width());
        let y0 = ((1.0 - v1.min(1.0)) * h).floor() as u32;
        let y1 = (((1.0 - v0.max(0.0)) * h).ceil() as u32).min(rgb.height());
        for y in y0..y1 {
            for x in x0..x1 {
                rgb.put_pixel(x, y, image::Rgb(fill));
            }
        }
        let old = self.doc.get_object(xid)?.as_stream()?.clone();
        let jpeg = old.dict.get(b"Filter").ok().map(|f| format!("{f:?}")).is_some_and(|f| f.contains("DCTDecode"));
        let mut dict = Dictionary::new();
        for key in [&b"Interpolate"[..], b"SMask", b"Intent"] {
            if let Ok(v) = old.dict.get(key) {
                dict.set(key.to_vec(), v.clone());
            }
        }
        Ok(self.doc.add_object(image_stream(dict, &image::DynamicImage::ImageRgb8(rgb), jpeg)?))
    }

    /// Changes a picture.
    fn image(&mut self, plan: &mut PagePlan, a: &Analysis, path: &[usize], change: &ImageChange) -> anyhow::Result<()> {
        let img = a.po.images.iter().find(|i| i.path == path).ok_or_else(|| anyhow!("picture not found"))?;
        let container_path = path[..path.len() - 1].to_vec();
        let info = a.po.container(&container_path).ok_or_else(|| anyhow!("picture not found"))?;
        let k = info.images.iter().position(|i| Some(i) == path.last()).ok_or_else(|| anyhow!("picture not found"))?;
        let c = self.container(plan, &a.po, &container_path)?;
        let op_index = *plan.containers[c].scan.image_ops.get(k).ok_or_else(|| anyhow!("could not find the picture in the content"))?;
        let op = plan.containers[c].scan.ops[op_index].clone();
        let original = String::from_utf8_lossy(&plan.containers[c].content[op.start..op.end]).into_owned();
        let m_page = mul(&img.matrix, &img.to_page);
        // Draws `what` with the unit square mapped by `target` (page space).
        let placed = |target: &Mat, what: &str| -> anyhow::Result<String> {
            let to_c = invert(&img.to_page).ok_or_else(|| anyhow!("degenerate matrix"))?;
            let m_c = mul(target, &to_c);
            let adj = mul(&m_c, &invert(&img.matrix).ok_or_else(|| anyhow!("degenerate picture"))?);
            Ok(format!("q {} cm {what} Q", fmt_mat(&adj)))
        };
        let text = match change {
            ImageChange::Delete => String::new(),
            ImageChange::Place { x, y, w, h } => {
                let target = display_box_to_page(&a.tf, [*x, *y, *w, *h]);
                placed(&rect_map(&m_page, img.rect, target), &original)?
            }
            ImageChange::Replace { file } => {
                let (id, (iw, ih)) = add_image_file(&mut self.doc, file)?;
                let name = plan.containers[c].resource_name(&self.doc, b"XObject", id);
                // Fit inside the old box, keeping the new picture's proportions.
                let (bw, bh) = (img.rect[2] - img.rect[0], img.rect[3] - img.rect[1]);
                let s = (bw / iw as f64).min(bh / ih as f64);
                let (nw, nh) = (iw as f64 * s, ih as f64 * s);
                let (cx, cy) = ((img.rect[0] + img.rect[2]) / 2.0, (img.rect[1] + img.rect[3]) / 2.0);
                let target = [nw, 0.0, 0.0, nh, cx - nw / 2.0, cy - nh / 2.0];
                placed(&target, &format!("{} Do", write_name(&name)))?
            }
        };
        plan.containers[c].splices.push((op.start, op.end, Out::Parts(vec![Part::Raw(text)])));
        Ok(())
    }

    /// Adds a picture in a box of the displayed page.
    fn insert_image(&mut self, plan: &mut PagePlan, a: &Analysis, bx: [f32; 4], file: &Path) -> anyhow::Result<()> {
        let (id, _) = add_image_file(&mut self.doc, file)?;
        let c = self.container(plan, &a.po, &[])?;
        let name = plan.containers[c].resource_name(&self.doc, b"XObject", id);
        let (dw, dh) = (a.tf.width(), a.tf.height());
        // Upright as displayed: x along the display's right, y up the display.
        let o = a.tf.to_user(bx[0] * dw, (bx[1] + bx[3]) * dh);
        let r = a.tf.to_user((bx[0] + bx[2]) * dw, (bx[1] + bx[3]) * dh);
        let u = a.tf.to_user(bx[0] * dw, bx[1] * dh);
        let m: Mat = [(r.0 - o.0) as f64, (r.1 - o.1) as f64, (u.0 - o.0) as f64, (u.1 - o.1) as f64, o.0 as f64, o.1 as f64];
        plan.containers[c].appended.push(Out::Parts(vec![Part::Raw(format!("q {} cm {} Do Q\n", fmt_mat(&m), write_name(&name)))]));
        Ok(())
    }

    /// Removes the outline shapes of letters in a box and writes `text` there instead.
    #[allow(clippy::too_many_arguments)]
    fn outlined(&mut self, plan: &mut PagePlan, a: &Analysis, bx: [f32; 4], baseline: f32, text: &str, size: f32, color: [u8; 3], style: &NewTextStyle, fit: bool) -> anyhow::Result<()> {
        let (dw, dh) = (a.tf.width() as f64, a.tf.height() as f64);
        let size = size as f64;
        let margin = 0.15 * size;
        let (x0, y0) = (bx[0] as f64 * dw - margin, bx[1] as f64 * dh - margin);
        let (x1, y1) = ((bx[0] + bx[2]) as f64 * dw + margin, (bx[1] + bx[3]) as f64 * dh + margin);
        let inside = |r: [f64; 4]| {
            // `r` in page space: its corners on the display.
            let p = a.tf.point(r[0] as f32, r[1] as f32);
            let q = a.tf.point(r[2] as f32, r[3] as f32);
            let (l, t, rr, b) = (p.0.min(q.0) as f64, p.1.min(q.1) as f64, p.0.max(q.0) as f64, p.1.max(q.1) as f64);
            l >= x0 && rr <= x1 && t >= y0 && b <= y1
        };
        let containers: Vec<(ObjPath, Mat)> = a.po.containers.iter().map(|c| (c.path.clone(), c.to_page)).collect();
        let mut removed = 0;
        for (path, to_page) in containers {
            let Ok(c) = self.container(plan, &a.po, &path) else { continue };
            let cp = &mut plan.containers[c];
            for paint in content::scan_paths(&cp.scan.ops, cp.matrix) {
                if paint.clip {
                    continue;
                }
                let hits: Vec<bool> = paint.subpaths.iter().map(|sp| inside(objects::transform_rect(&to_page, sp.2))).collect();
                if !hits.iter().any(|h| *h) {
                    continue;
                }
                for (sp, hit) in paint.subpaths.iter().zip(&hits) {
                    if *hit {
                        cp.splices.push((sp.0, sp.1, Out::Parts(vec![Part::Raw(String::new())])));
                        removed += 1;
                    }
                }
                if hits.iter().all(|h| *h) {
                    cp.splices.push((paint.paint.0, paint.paint.1, Out::Parts(vec![Part::Raw(String::new())])));
                }
            }
        }
        if removed == 0 {
            bail!("the outlined letters were not found in the page content");
        }
        let max_width = fit.then_some(bx[2] as f64 * dw);
        self.draw_text(plan, a, bx[0] as f64 * dw, baseline as f64 * dh, text, size, color, style, max_width)
    }

    fn restyle(&mut self, plan: &mut PagePlan, a: &Analysis, objects: &[ObjPath], text: Option<&str>, change: &StyleChange, fit: bool) -> anyhow::Result<()> {
        let (c, shows) = self.shows_of(plan, a, objects)?;
        let page = plan.page;
        let first = plan.containers[c].scan.shows[shows[0]].clone();
        let r = a.run_of(&objects[0]).ok_or_else(|| anyhow!("text run not found"))?;
        let o = &a.po.objs[a.runs[r][0]];
        let text = text.map(str::to_string).unwrap_or_else(|| a.texts[r].clone());
        if text.trim().is_empty() {
            return self.delete(plan, a, objects);
        }
        let meta_info = plan.containers[c].fonts.get(&content::decode_name(&first.font)).map(|m| (m.family.clone(), m.base.clone(), m.bold, m.italic));
        let (family, base, font_bold, font_italic) = meta_info.unwrap_or_else(|| ("Arial".into(), String::new(), o.bold_flag, o.italic_flag));
        let cur_bold = font_bold || o.fill_stroke;
        let cur_italic = font_italic || o.skewed();
        let want_bold = change.bold.unwrap_or(cur_bold);
        let want_italic = change.italic.unwrap_or(cur_italic);

        // The font: the run's own when its style already matches, else the
        // installed face of the family, else synthetic bold/italic.
        let mut synth_bold = false;
        let mut synth_italic = false;
        let (fb, pieces) = if want_bold == font_bold && want_italic == font_italic {
            self.pieces_for(page, plan, c, &first, o, &text)?
        } else if let Some(f) = installed_variant(&family, want_bold, want_italic, &text) {
            let fb = self.ctx.fallback(&family, f);
            (Some(fb), fallback_pieces(&mut self.ctx, fb, &text))
        } else {
            if (!want_bold && font_bold) || (!want_italic && font_italic) {
                bail!("no {} style of {} is installed", if !want_bold && font_bold { "regular" } else { "upright" }, if base.is_empty() { &family } else { &base });
            }
            synth_bold = want_bold && !font_bold;
            synth_italic = want_italic && !font_italic;
            self.pieces_for(page, plan, c, &first, o, &text)?
        };
        let k = change.size.map(|s| s as f64 / o.size.max(0.1) as f64).unwrap_or(1.0);
        let size = first.size * k;
        let mut tm = first.tm();
        if o.skewed() && !want_italic {
            // Undo a synthetic slant: make the vertical axis perpendicular again.
            let len_ab = (tm[0] * tm[0] + tm[1] * tm[1]).sqrt().max(1e-9);
            let len_cd = (tm[2] * tm[2] + tm[3] * tm[3]).sqrt();
            tm[2] = -tm[1] / len_ab * len_cd;
            tm[3] = tm[0] / len_ab * len_cd;
        }
        if synth_italic && !o.skewed() {
            tm = mul(&[1.0, 0.0, 0.2126, 1.0, 0.0, 0.0], &tm);
        }
        let color = change.color.or(if synth_bold { Some(o.color) } else { None });
        let tw = if fb.is_some() { 0.0 } else { first.tw };
        let new_adv = pieces_advance(&pieces, size, first.tc, tw, first.th);
        let m = mul(&first.tm(), &first.ctm);
        let pobjs: Vec<&PObj> = shows.iter().filter_map(|k| a.po.find(&objects_path(&plan.containers[c], *k))).collect();
        let ext = pobjs.iter().map(|oo| objects::extent(o, oo)).fold(0.0f64, f64::max);
        let old_adv = ext / x_scale(&m).max(1e-9);
        let run = SegRun {
            font: Some(fb.map(RunFont::Fallback).unwrap_or(RunFont::Res(first.font.clone()))),
            size,
            color,
            stroke: synth_bold.then(|| 0.03 * size * x_scale(&tm)),
            render: (o.fill_stroke && !want_bold).then_some(0),
            tz: fit_scale(fit, first.th, old_adv, new_adv),
            tm: Some({
                let shown = new_adv * fit_scale(fit, first.th, old_adv, new_adv).map(|t| t / first.th).unwrap_or(1.0);
                mul(&translate(anchor_shift(a, Some(r), &text, old_adv, shown), 0.0), &tm)
            }),
            items: pieces.into_iter().map(Item::Piece).collect(),
            base_rise: first.rise,
            ..Default::default()
        };
        self.place_segment(plan, c, &shows, Segment { prefix: quote_prefix(&first, false), reset: false, runs: vec![run], resume: resume(&first) })
    }

    /// Moves objects by `v` (page space), keeping their glyphs and spacing exactly.
    fn move_objects(&mut self, plan: &mut PagePlan, a: &Analysis, objects: &[ObjPath], v: (f64, f64)) -> anyhow::Result<()> {
        let (c, shows) = self.shows_of(plan, a, objects)?;
        let to_page = a.po.container(&plan.containers[c].path).map(|i| i.to_page).unwrap_or(IDENTITY);
        let inv = invert(&to_page).ok_or_else(|| anyhow!("degenerate form matrix"))?;
        let vc = apply_vec(&inv, v.0, v.1);
        let cp = &mut plan.containers[c];
        for k in shows {
            let sh = cp.scan.shows[k].clone();
            let inv_ctm = invert(&sh.ctm).ok_or_else(|| anyhow!("degenerate matrix"))?;
            let vt = apply_vec(&inv_ctm, vc.0, vc.1);
            let tm = mul(&sh.tm(), &translate(vt.0, vt.1));
            let run = SegRun { font: None, size: sh.size, tm: Some(tm), items: sh.raw_tokens().into_iter().map(Item::Tok).collect(), base_rise: sh.rise, ..Default::default() };
            let op = &cp.scan.ops[sh.op];
            cp.splices.push((op.start, op.end, Out::Seg(Segment { prefix: quote_prefix(&sh, false), reset: false, runs: vec![run], resume: resume(&sh) })));
        }
        Ok(())
    }

    /// Lays a paragraph out again with new text. `tab` holds the lines that
    /// `para.lines` refers to. A `\n` in `text` starts a new line.
    fn layout_paragraph(&mut self, plan: &mut PagePlan, a: &Analysis, tab: &[Line], para: &Para, text: &str, opt: &Wrap) -> anyhow::Result<Wrapped> {
        let text = text.trim_matches('\n');
        let runs: Vec<usize> = para.lines.iter().flat_map(|l| tab[*l].runs.iter().copied()).collect();
        let paths: Vec<ObjPath> = runs.iter().flat_map(|r| a.run_paths(*r)).collect();
        let (c, shows) = self.shows_of(plan, a, &paths)?;
        let page = plan.page;
        let anchor = plan.containers[c].scan.shows[shows[0]].clone();
        let ma = mul(&anchor.tm(), &anchor.ctm);
        let ia = invert(&ma).ok_or_else(|| anyhow!("degenerate text matrix"))?;
        let scale_a = x_scale(&ma).max(1e-9);
        let to_page = a.po.container(&plan.containers[c].path).map(|i| i.to_page).unwrap_or(IDENTITY);
        let inv_page = invert(&to_page).ok_or_else(|| anyhow!("degenerate form matrix"))?;
        // Displayed page → the anchor's text space.
        let to_text = |x: f64, y: f64| {
            let p = a.tf.to_user(x as f32, y as f32);
            let q = apply(&inv_page, p.0 as f64, p.1 as f64);
            apply(&ia, q.0, q.1)
        };

        // Geometry of the original lines in the anchor's text space.
        struct L {
            left: f64,
            right: f64,
            base: f64,
        }
        let mut lines: Vec<L> = Vec::new();
        for l in &para.lines {
            let (mut left, mut right) = (f64::MAX, f64::MIN);
            let mut base = None;
            for r in &tab[*l].runs {
                for k in &a.runs[*r] {
                    let o = &a.po.objs[*k];
                    for cn in o.corners() {
                        let p = apply(&ia, cn.0, cn.1);
                        left = left.min(p.0);
                        right = right.max(p.0);
                    }
                    if base.is_none() {
                        base = Some(apply(&ia, o.origin.0, o.origin.1).1);
                    }
                }
            }
            lines.push(L { left, right, base: base.unwrap_or(0.0) });
        }
        let mut left = lines.iter().map(|l| l.left).fold(f64::MAX, f64::min);
        let mut right = lines.iter().map(|l| l.right).fold(f64::MIN, f64::max);
        let rtl_para = objects::first_strong_rtl(&para.text);
        let mut indent = match (para.align, rtl_para) {
            (Align::Left | Align::Justify, false) => lines[0].left - left,
            (Align::Right | Align::Justify, true) => right - lines[0].right,
            _ => 0.0,
        };
        // A frame (a table cell) sets the width and the lowest baseline.
        let mut floor = None;
        if let Some([fl, ft, fr, fb]) = opt.frame {
            let y = (ft + fb) / 2.0;
            let (p, q) = (to_text(fl, y), to_text(fr, y));
            left = p.0.min(q.0);
            right = p.0.max(q.0);
            indent = 0.0;
            floor = Some(to_text(fl, fb).1);
        }
        let mut pitches: Vec<f64> = lines.windows(2).map(|w| w[0].base - w[1].base).filter(|d| *d > 0.0).collect();
        pitches.sort_by(|x, y| x.total_cmp(y));

        // Styles: one per run.
        struct St {
            font: Vec<u8>,
            size: f64,
            color: [u8; 3],
            stroke: bool,
            skew: bool,
            obj: usize,
        }
        let mut styles: Vec<St> = Vec::new();
        let mut style_of_run: HashMap<usize, usize> = HashMap::new();
        for r in &runs {
            let o = &a.po.objs[a.runs[*r][0]];
            let k = plan.containers[c].show_of(o.index()).ok_or_else(|| anyhow!("text object not in content"))?;
            let sh = &plan.containers[c].scan.shows[k];
            let size_c = o.size as f64 / x_len(&o.to_page).max(1e-9);
            style_of_run.insert(*r, styles.len());
            styles.push(St { font: sh.font.clone(), size: size_c / scale_a, color: o.color, stroke: o.fill_stroke, skew: o.skewed(), obj: a.runs[*r][0] });
        }
        let max_size = styles.iter().map(|s| s.size).fold(0.0f64, f64::max).max(0.1);
        let pitch = pitches.get(pitches.len() / 2).copied().unwrap_or(1.2 * max_size);

        // Styles of the new characters: unchanged ones keep theirs, new ones
        // take the style of the character before them.
        let mut old_styles: Vec<usize> = Vec::new();
        for s in &para.src {
            let st = match s {
                Src::Run(r, _) => style_of_run.get(r).copied().unwrap_or(0),
                Src::Join => old_styles.last().copied().unwrap_or(0),
            };
            old_styles.push(st);
        }
        let old: Vec<char> = para.text.chars().collect();
        let new: Vec<char> = text.chars().collect();
        let mut p = 0;
        while p < old.len() && p < new.len() && old[p] == new[p] {
            p += 1;
        }
        let mut sfx = 0;
        while sfx < old.len() - p && sfx < new.len() - p && old[old.len() - 1 - sfx] == new[new.len() - 1 - sfx] {
            sfx += 1;
        }
        let mut new_styles = Vec::with_capacity(new.len());
        for i in 0..new.len() {
            let st = if i < p {
                old_styles[i]
            } else if i >= new.len() - sfx {
                old_styles[old.len() - (new.len() - i)]
            } else if p > 0 {
                old_styles[p - 1]
            } else {
                old_styles.get(old.len() - sfx).copied().unwrap_or(0)
            };
            new_styles.push(st);
        }

        // Words: pieces per style part, the width of the space before them,
        // and the line breaks before them.
        struct WPart {
            style: usize,
            text: String,
            fb: Option<usize>,
            pieces: Vec<Piece>,
            width: f64,
        }
        struct Word {
            parts: Vec<WPart>,
            width: f64,
            space: f64,
            brk: usize,
        }
        let mut words: Vec<Word> = Vec::new();
        let mut i = 0;
        let mut space = 0.0;
        let mut brk = 0;
        let mut space_cache: HashMap<usize, f64> = HashMap::new();
        while i < new.len() {
            if new[i] == '\n' {
                brk += 1;
                i += 1;
                continue;
            }
            if new[i].is_whitespace() {
                let st = new_styles[i];
                let em = match space_cache.get(&st) {
                    Some(v) => *v,
                    None => {
                        let v = match plan.containers[c].fonts.get(&content::decode_name(&styles[st].font)) {
                            Some(meta) => fonts::space_em(&mut self.ctx, &self.doc, meta),
                            None => 0.28,
                        };
                        space_cache.insert(st, v);
                        v
                    }
                };
                space += em * styles[st].size;
                i += 1;
                continue;
            }
            let mut j = i;
            while j < new.len() && !new[j].is_whitespace() {
                j += 1;
            }
            let mut parts = Vec::new();
            let mut s = i;
            while s < j {
                let st = new_styles[s];
                let mut e = s;
                while e < j && new_styles[e] == st {
                    e += 1;
                }
                let chunk: String = new[s..e].iter().collect();
                let o = &a.po.objs[styles[st].obj];
                let k = plan.containers[c].show_of(o.index()).unwrap();
                let sh = plan.containers[c].scan.shows[k].clone();
                let (fb, pieces) = self.pieces_for(page, plan, c, &sh, o, &chunk)?;
                let width = pieces_advance(&pieces, styles[st].size, 0.0, 0.0, 1.0);
                parts.push(WPart { style: st, text: chunk, fb, pieces, width });
                s = e;
            }
            let width = parts.iter().map(|p| p.width).sum();
            words.push(Word { parts, width, space: if words.is_empty() { 0.0 } else { space }, brk });
            space = 0.0;
            brk = 0;
            i = j;
        }

        // Break into lines, smaller when asked to fit.
        let avail = |line: usize, k: f64| (right - left - if line == 0 { indent } else { 0.0 }) / k;
        let break_lines = |k: f64| -> Vec<Vec<usize>> {
            let mut out: Vec<Vec<usize>> = Vec::new();
            let mut cur: Vec<usize> = Vec::new();
            let mut w = 0.0;
            for (wi, word) in words.iter().enumerate() {
                if word.brk > 0 {
                    out.push(std::mem::take(&mut cur));
                    for _ in 1..word.brk {
                        out.push(Vec::new());
                    }
                    w = 0.0;
                }
                let add = if cur.is_empty() { word.width } else { word.space + word.width };
                if !cur.is_empty() && w + add > avail(out.len(), k) + 1e-6 {
                    out.push(std::mem::take(&mut cur));
                    w = word.width;
                } else {
                    w += add;
                }
                cur.push(wi);
            }
            if !cur.is_empty() {
                out.push(cur);
            }
            out
        };
        // Lines that fit: the frame's height, or as many as before.
        let max_lines = |k: f64| match floor {
            Some(f) => ((lines[0].base - f - 0.25 * max_size * k) / (pitch * k)).floor().max(0.0) as usize + 1,
            None => lines.len(),
        };
        let mut k = 1.0;
        let mut broken = break_lines(k);
        if opt.fit {
            let min_k = if floor.is_some() { 0.6 } else { 0.7 };
            while broken.len() > max_lines(k) && k > min_k + 1e-9 {
                k -= 0.05;
                broken = break_lines(k);
            }
        }
        let overfull = floor.is_some() && broken.len() > max_lines(k);

        // Place the words, in display order.
        let word_dir: Vec<Option<bool>> = words
            .iter()
            .map(|w| {
                let t: String = w.parts.iter().map(|p| p.text.as_str()).collect();
                t.chars().find(|c| c.is_alphabetic()).map(is_rtl)
            })
            .collect();
        let mut out_lines = Vec::new();
        for (li, ws) in broken.iter().enumerate() {
            let y = lines[0].base - li as f64 * pitch * k;
            let mut seg_runs = Vec::new();
            let natural: f64 = ws.iter().enumerate().map(|(n, w)| words[*w].width + if n > 0 { words[*w].space } else { 0.0 }).sum::<f64>() * k;
            let width = avail(li, k) * k;
            // The last line of the paragraph, or before a line break, is not justified.
            let last = broken.get(li + 1).is_none_or(|n| n.first().is_none_or(|w| words[*w].brk > 0));
            let gaps = ws.len().saturating_sub(1);
            let extra = if para.align == Align::Justify && !last && gaps > 0 { ((width - natural) / gaps as f64).max(0.0) } else { 0.0 };
            let first_indent = if li == 0 { indent } else { 0.0 };
            let mut x = match (para.align, rtl_para) {
                (Align::Center, _) => (left + right) / 2.0 - natural / 2.0,
                (Align::Right, _) | (Align::Justify, true) if extra == 0.0 => right - first_indent - natural,
                (_, true) => left,
                _ => left + first_indent,
            };
            let order = visual_order_of(&ws.iter().map(|w| word_dir[*w]).collect::<Vec<_>>(), rtl_para);
            for (n, &vi) in order.iter().enumerate() {
                let w = ws[vi];
                if n > 0 {
                    let prev = ws[order[n - 1]];
                    x += words[w].space.max(words[prev].space) * k + extra;
                }
                let rtl_word = word_dir[w] == Some(true);
                let parts: Vec<&WPart> = if rtl_word { words[w].parts.iter().rev().collect() } else { words[w].parts.iter().collect() };
                for part in parts {
                    let st = &styles[part.style];
                    let mut tm = mul(&translate(x, y), &anchor.tm());
                    if st.skew {
                        tm = mul(&[1.0, 0.0, 0.2126, 1.0, 0.0, 0.0], &tm);
                    }
                    let size = st.size * k;
                    seg_runs.push(SegRun {
                        font: Some(part.fb.map(RunFont::Fallback).unwrap_or_else(|| RunFont::Res(st.font.clone()))),
                        size,
                        color: Some(st.color),
                        stroke: st.stroke.then(|| 0.03 * size * x_scale(&tm)),
                        render: if st.stroke { None } else { Some(0) },
                        tz: None,
                        tm: Some(tm),
                        items: part.pieces.iter().cloned().map(Item::Piece).collect(),
                        base_rise: 0.0,
                        ..Default::default()
                    });
                    x += part.width * k;
                }
            }
            out_lines.push(seg_runs);
        }
        Ok(Wrapped { c, shows, anchor, ma, to_page, lines: out_lines, pitch: pitch * k, old_lines: lines.len(), runs, overfull })
    }

    /// Writes the lines of a laid-out paragraph in place of the old ones.
    fn place_wrapped(&mut self, plan: &mut PagePlan, w: &Wrapped) -> anyhow::Result<()> {
        let runs: Vec<SegRun> = w.lines.iter().flatten().cloned().collect();
        self.place_segment(plan, w.c, &w.shows, Segment { prefix: quote_prefix(&w.anchor, false), reset: true, runs, resume: resume(&w.anchor) })
    }

    /// The displayed-page vector of moving a laid-out line by `dy` (anchor text space).
    fn wrapped_shift(a: &Analysis, w: &Wrapped, dy: f64) -> ((f64, f64), f64) {
        let vc = apply_vec(&w.ma, 0.0, dy);
        let vp = apply_vec(&w.to_page, vc.0, vc.1);
        let d0 = a.tf.point(0.0, 0.0);
        let d1 = a.tf.point(vp.0 as f32, vp.1 as f32);
        (vp, (d1.1 - d0.1) as f64)
    }

    fn paragraph(&mut self, plan: &mut PagePlan, a: &Analysis, objects: &[ObjPath], text: &str, opt: Wrap, touched: &mut HashSet<ObjPath>) -> anyhow::Result<()> {
        let r0 = a.run_of(objects.first().ok_or_else(|| anyhow!("no text objects given"))?).ok_or_else(|| anyhow!("text run not found"))?;
        let pi = a.para_of_run(r0).ok_or_else(|| anyhow!("paragraph not found"))?;
        let para = &a.paras[pi];
        let mut w = self.layout_paragraph(plan, a, &a.lines, para, text, &opt)?;
        for r in &w.runs {
            for p in a.run_paths(*r) {
                touched.insert(p);
            }
        }
        let page = plan.page;

        // More lines than before: move the text below down, when there is room.
        let grown = w.lines.len() as f64 - w.old_lines as f64;
        if grown > 0.0 && !opt.fit {
            let (vp, shift_d) = Self::wrapped_shift(a, &w, -grown * w.pitch);
            let pitch_d = shift_d.abs() / grown.max(1.0);
            let moved = if opt.push_down || opt.flow { self.text_below(a, para, &w.runs, shift_d.abs(), pitch_d.max(1.0)) } else { None };
            match moved {
                Some(below) => {
                    for r in below {
                        let paths = a.run_paths(r);
                        if paths.iter().any(|p| touched.contains(p)) {
                            continue;
                        }
                        for p in &paths {
                            touched.insert(p.clone());
                        }
                        self.move_objects(plan, a, &paths, vp)?;
                    }
                }
                None if opt.flow => {
                    self.flow(plan, a, para, &mut w, touched)?;
                }
                None => self.ctx.note(format!("Page {}: the paragraph is now longer and may overlap what follows it.", page + 1)),
            }
        }
        self.place_wrapped(plan, &w)
    }

    /// Runs below a paragraph in its column that can move down by `shift`
    /// (display points), or `None` when there is no room.
    fn text_below(&self, a: &Analysis, para: &Para, own: &[usize], shift: f64, pitch: f64) -> Option<Vec<usize>> {
        let (cl, cr) = (para.left, para.right);
        let overlaps = |l: f64, r: f64| r.min(cr) - l.max(cl) > 0.3 * (r - l).max(1.0);
        let mut cands: Vec<usize> = (0..a.runs.len())
            .filter(|r| !own.contains(r) && a.geoms[*r].top >= para.bottom - 1.0 && overlaps(a.geoms[*r].left, a.geoms[*r].right))
            .collect();
        cands.sort_by(|x, y| a.geoms[*x].top.total_cmp(&a.geoms[*y].top));
        let obstacles: Vec<(f64, f64, f64, f64)> = a
            .po
            .obstacles
            .iter()
            .map(|r| {
                let p = a.tf.point(r[0] as f32, r[1] as f32);
                let q = a.tf.point(r[2] as f32, r[3] as f32);
                (p.0.min(q.0) as f64, p.1.min(q.1) as f64, p.0.max(q.0) as f64, p.1.max(q.1) as f64)
            })
            .filter(|o| overlaps(o.0, o.2) && o.1 >= para.bottom - 1.0 && (o.3 - o.1) < 0.9 * a.tf.height() as f64)
            .collect();
        let mut block = Vec::new();
        let mut bottom = para.bottom;
        let mut limit = a.tf.height() as f64 - 18.0;
        for r in cands {
            let g = &a.geoms[r];
            if let Some(ob) = obstacles.iter().filter(|o| o.1 >= bottom - 1.0 && o.1 < g.top).map(|o| o.1).reduce(f64::min) {
                limit = limit.min(ob);
                break;
            }
            if g.top - bottom > 3.0 * pitch {
                limit = limit.min(g.top);
                break;
            }
            block.push(r);
            bottom = bottom.max(g.bottom);
        }
        if let Some(ob) = obstacles.iter().filter(|o| o.1 >= bottom - 1.0).map(|o| o.1).reduce(f64::min) {
            limit = limit.min(ob);
        }
        (bottom + shift <= limit).then_some(block)
    }

    /// Embeds fonts, writes the new content streams, and returns the document.
    fn finish(mut self) -> anyhow::Result<lopdf::Document> {
        for i in 0..self.ctx.fallbacks.len() {
            if self.ctx.fallbacks[i].glyphs.is_empty() {
                continue;
            }
            fonts::embed_fallback(&mut self.doc, &mut self.ctx.fallbacks[i])?;
        }
        let additions = std::mem::take(&mut self.ctx.additions);
        fonts::apply_additions(&mut self.doc, &additions)?;
        let plans = std::mem::take(&mut self.plans);
        for mut plan in plans {
            // Deepest content streams first: a changed form gets a new name in its parent.
            let mut order: Vec<usize> = (0..plan.containers.len()).collect();
            order.sort_by_key(|i| std::cmp::Reverse(plan.containers[*i].path.len()));
            for i in order {
                let changed = !plan.containers[i].splices.is_empty() || !plan.containers[i].appended.is_empty();
                if !changed {
                    continue;
                }
                let content = self.render(&mut plan.containers[i])?;
                let cp = &plan.containers[i];
                if cp.path.is_empty() {
                    let mut stream = Stream::new(Dictionary::new(), content);
                    let _ = stream.compress();
                    let id = self.doc.add_object(stream);
                    let res = cp.res_dirty.then(|| cp.resources.clone());
                    let pd = self.doc.get_dictionary_mut(plan.page_id)?;
                    pd.set("Contents", id);
                    if let Some(res) = res {
                        pd.set("Resources", res);
                    }
                } else {
                    // A copy of the form XObject, used only here.
                    let xid = cp.xobject.ok_or_else(|| anyhow!("form without XObject"))?;
                    let mut dict = self.doc.get_object(xid)?.as_stream()?.dict.clone();
                    dict.remove(b"Filter");
                    dict.remove(b"DecodeParms");
                    dict.remove(b"Length");
                    if cp.res_dirty || dict.has(b"Resources") || !cp.resources.is_empty() {
                        dict.set("Resources", cp.resources.clone());
                    }
                    let mut stream = Stream::new(dict, content);
                    let _ = stream.compress();
                    let new_id = self.doc.add_object(stream);
                    let path = cp.path.clone();
                    let parent_path = path[..path.len() - 1].to_vec();
                    let parent = plan.containers.iter().position(|c| c.path == parent_path).ok_or_else(|| anyhow!("parent content not found"))?;
                    let rank = cp.rank;
                    let pc = &mut plan.containers[parent];
                    let name = pc.resource_name(&self.doc, b"XObject", new_id);
                    let op_index = *pc.scan.form_dos.get(rank).ok_or_else(|| anyhow!("form not found in content"))?;
                    let op = &pc.scan.ops[op_index];
                    let (s, e) = (op.start, op.end);
                    pc.splices.push((s, e, Out::Parts(vec![Part::Raw(format!("{} Do", write_name(&name)))])));
                }
            }
        }
        Ok(self.doc)
    }

    fn font_name(&self, cp: &mut CPlan, f: &RunFont) -> anyhow::Result<Vec<u8>> {
        match f {
            RunFont::Res(n) => Ok(n.clone()),
            RunFont::Fallback(i) => {
                let id = self.ctx.fallbacks[*i].id.ok_or_else(|| anyhow!("font was not embedded"))?;
                Ok(cp.resource_name(&self.doc, b"Font", id))
            }
            RunFont::Obj(id) => Ok(cp.resource_name(&self.doc, b"Font", *id)),
        }
    }

    fn toks(&self, items: &[Item]) -> Vec<Tok> {
        let mut out = Vec::new();
        let mut rise = 0.0;
        for it in items {
            match it {
                Item::Tok(t) => out.push(t.clone()),
                Item::Piece(Piece::Gap(em)) => out.push(Tok::Kern(-em * 1000.0)),
                Item::Piece(Piece::Glyph { code, fb, em, adv, dx, dy, .. }) => {
                    let code = match fb {
                        Some(i) => {
                            let g = u16::from_be_bytes([code[0], code[1]]);
                            let n = self.ctx.fallbacks[*i].map.get(&g).copied().unwrap_or(0);
                            n.to_be_bytes().to_vec()
                        }
                        None => code.clone(),
                    };
                    if (dy - rise).abs() > 1e-6 {
                        out.push(Tok::Rise(*dy));
                        rise = *dy;
                    }
                    if dx.abs() > 1e-6 {
                        out.push(Tok::Kern(-dx * 1000.0));
                    }
                    out.push(Tok::Code(code));
                    let post = adv - dx - em;
                    if post.abs() > 1e-6 {
                        out.push(Tok::Kern(-post * 1000.0));
                    }
                }
            }
        }
        if rise.abs() > 1e-6 {
            out.push(Tok::Rise(0.0));
        }
        out
    }

    fn render_runs(&self, cp: &mut CPlan, seg: &Segment) -> anyhow::Result<String> {
        let mut s = String::new();
        if seg.reset {
            s.push_str("0 Tc 0 Tw 100 Tz 0 Ts ");
        }
        for run in &seg.runs {
            if let Some(tm) = &run.tm {
                s.push_str(&format!("{} Tm ", fmt_mat(tm)));
            }
            if let Some(f) = &run.font {
                let name = self.font_name(cp, f)?;
                s.push_str(&format!("{} {} Tf ", write_name(&name), fmt(run.size)));
            }
            if let Some([r, g, b]) = run.color {
                let (r, g, b) = (fmt(r as f64 / 255.0), fmt(g as f64 / 255.0), fmt(b as f64 / 255.0));
                s.push_str(&format!("{r} {g} {b} rg "));
                if run.stroke.is_some() {
                    s.push_str(&format!("{r} {g} {b} RG "));
                }
            }
            if let Some(w) = run.stroke {
                s.push_str(&format!("2 Tr {} w ", fmt(w)));
            } else if let Some(m) = run.render {
                s.push_str(&format!("{m} Tr "));
            }
            if let Some(tz) = run.tz {
                s.push_str(&format!("{} Tz ", fmt(tz * 100.0)));
            }
            if let Some(tc) = run.tc {
                s.push_str(&format!("{} Tc ", fmt(tc)));
            }
            if let Some(tw) = run.tw {
                s.push_str(&format!("{} Tw ", fmt(tw)));
            }
            s.push_str(&tj_ops(&self.toks(&run.items), run.size, run.base_rise));
            s.push(' ');
        }
        Ok(s)
    }

    fn render_out(&self, cp: &mut CPlan, out: &Out) -> anyhow::Result<String> {
        Ok(match out {
            Out::Parts(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        Part::Raw(t) => s.push_str(t),
                        Part::Tf(f, size) => {
                            let name = self.font_name(cp, f)?;
                            s.push_str(&format!(" {} {} Tf ", write_name(&name), fmt(*size)));
                        }
                        Part::Tj(items, size, rise) => s.push_str(&tj_ops(&self.toks(items), *size, *rise)),
                    }
                }
                s
            }
            Out::Seg(seg) => {
                let body = self.render_runs(cp, seg)?;
                format!("{}ET q BT {body}ET Q {}", seg.prefix, seg.resume)
            }
            Out::Block(seg) => {
                let body = self.render_runs(cp, seg)?;
                format!("q BT {body}ET Q\n")
            }
        })
    }

    fn render(&self, cp: &mut CPlan) -> anyhow::Result<Vec<u8>> {
        let mut splices = std::mem::take(&mut cp.splices);
        splices.sort_by_key(|s| s.0);
        let appended = std::mem::take(&mut cp.appended);
        let mut out: Vec<u8> = Vec::with_capacity(cp.content.len() + 256);
        let wrap = !appended.is_empty();
        let k = if wrap { 1 + (-cp.scan.min_depth).max(0) } else { 0 };
        for _ in 0..k {
            out.extend_from_slice(b"q\n");
        }
        let mut pos = 0;
        let content = std::mem::take(&mut cp.content);
        for (start, end, o) in &splices {
            if *start < pos {
                bail!("overlapping edits");
            }
            out.extend_from_slice(&content[pos..*start]);
            let text = self.render_out(cp, o)?;
            if !text.is_empty() {
                // Operators written right after another token need a separator.
                out.push(b' ');
                out.extend_from_slice(text.as_bytes());
                out.push(b' ');
            }
            pos = *end;
        }
        out.extend_from_slice(&content[pos..]);
        if wrap {
            out.push(b'\n');
            for _ in 0..(k + cp.scan.end_depth).max(0) {
                out.extend_from_slice(b"Q\n");
            }
            for o in &appended {
                let text = self.render_out(cp, o)?;
                out.extend_from_slice(text.as_bytes());
            }
        }
        cp.content = content;
        Ok(out)
    }
}

/// How far a redrawn run moves along its baseline (text space) so that it
/// stays anchored like the original: right-to-left text keeps its right
/// end, a centred line its centre, anything else its start.
fn anchor_shift(a: &Analysis, run: Option<usize>, text: &str, old: f64, new: f64) -> f64 {
    if objects::first_strong_rtl(text) {
        return old - new;
    }
    let Some(r) = run else { return 0.0 };
    let g = &a.geoms[r];
    let alone = a.lines.iter().find(|l| l.runs.contains(&r)).is_some_and(|l| l.runs.len() == 1);
    let page_w = a.tf.width() as f64;
    let centred = alone && ((g.left + g.right) / 2.0 - page_w / 2.0).abs() < 0.015 * page_w && g.right - g.left < 0.9 * page_w;
    if centred {
        (old - new) / 2.0
    } else {
        0.0
    }
}

/// Display order of the words of a line (Unicode bidi rule L2 on word
/// levels): `dirs` gives each word's direction (`Some(true)` right to left,
/// `None` for numbers and punctuation).
fn visual_order_of(dirs: &[Option<bool>], rtl_para: bool) -> Vec<usize> {
    let base = if rtl_para { 1u8 } else { 0 };
    let strong = |d: bool| if d { 1 } else if base == 1 { 2 } else { 0 };
    let levels: Vec<u8> = (0..dirs.len())
        .map(|i| match dirs[i] {
            Some(d) => strong(d),
            None => {
                let prev = dirs[..i].iter().rev().flatten().next();
                let next = dirs[i + 1..].iter().flatten().next();
                match (prev, next) {
                    (Some(a), Some(b)) if a == b => strong(*a),
                    _ => base,
                }
            }
        })
        .collect();
    let mut order: Vec<usize> = (0..dirs.len()).collect();
    let max = levels.iter().copied().max().unwrap_or(0);
    let lowest_odd = levels.iter().copied().filter(|l| l % 2 == 1).min().unwrap_or(max + 1);
    let mut lvl = max;
    while lvl >= lowest_odd && lvl > 0 {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] >= lvl {
                let mut j = i;
                while j < order.len() && levels[order[j]] >= lvl {
                    j += 1;
                }
                order[i..j].reverse();
                i = j;
            } else {
                i += 1;
            }
        }
        lvl -= 1;
    }
    order
}

/// `m` (unit square → page) changed so that its bounds `from` become `to`.
fn rect_map(m: &Mat, from: [f64; 4], to: [f64; 4]) -> Mat {
    let sx = (to[2] - to[0]) / (from[2] - from[0]).max(1e-9);
    let sy = (to[3] - to[1]) / (from[3] - from[1]).max(1e-9);
    let a: Mat = [sx, 0.0, 0.0, sy, to[0] - from[0] * sx, to[1] - from[1] * sy];
    mul(m, &a)
}

/// An image XObject for `img` (JPEG or Flate-compressed samples, alpha as a soft mask).
fn image_stream(mut dict: Dictionary, img: &image::DynamicImage, jpeg: bool) -> anyhow::Result<Stream> {
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    dict.set("Width", img.width() as i64);
    dict.set("Height", img.height() as i64);
    dict.set("BitsPerComponent", 8i64);
    let gray = matches!(img.color(), image::ColorType::L8 | image::ColorType::L16 | image::ColorType::La8 | image::ColorType::La16);
    dict.set("ColorSpace", Object::Name(if gray { b"DeviceGray".to_vec() } else { b"DeviceRGB".to_vec() }));
    if jpeg {
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
        if gray {
            img.to_luma8().write_with_encoder(enc)?;
        } else {
            img.to_rgb8().write_with_encoder(enc)?;
        }
        dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        return Ok(Stream::new(dict, out).with_compression(false));
    }
    let samples = if gray { img.to_luma8().into_raw() } else { img.to_rgb8().into_raw() };
    let mut s = Stream::new(dict, samples);
    let _ = s.compress();
    Ok(s)
}

/// Adds an image file to the document as an image XObject; returns it and its size in pixels.
fn add_image_file(doc: &mut lopdf::Document, file: &Path) -> anyhow::Result<(ObjectId, (u32, u32))> {
    let (mut stream, mask, size) = load_image_file(file)?;
    if let Some(mask) = mask {
        let mid = doc.add_object(mask);
        stream.dict.set("SMask", mid);
    }
    Ok((doc.add_object(stream), size))
}

/// An image file as an image XObject (and its soft mask), and its size in pixels.
fn load_image_file(file: &Path) -> anyhow::Result<(Stream, Option<Stream>, (u32, u32))> {
    let bytes = std::fs::read(file).with_context(|| format!("could not read {}", file.display()))?;
    let img = image::load_from_memory(&bytes).with_context(|| format!("{} is not a picture this program can read", file.display()))?;
    let (w, h) = (img.width(), img.height());
    let is_jpeg = bytes.starts_with(&[0xFF, 0xD8]);
    let mut dict = Dictionary::new();
    let mut mask = None;
    let stream = if is_jpeg && matches!(img.color(), image::ColorType::Rgb8 | image::ColorType::L8) {
        // The JPEG as it is.
        dict.set("Type", Object::Name(b"XObject".to_vec()));
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Width", w as i64);
        dict.set("Height", h as i64);
        dict.set("BitsPerComponent", 8i64);
        dict.set("ColorSpace", Object::Name(if img.color() == image::ColorType::L8 { b"DeviceGray".to_vec() } else { b"DeviceRGB".to_vec() }));
        dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        Stream::new(dict, bytes).with_compression(false)
    } else {
        if img.color().has_alpha() {
            let alpha: Vec<u8> = img.to_rgba8().pixels().map(|p| p.0[3]).collect();
            let mut md = Dictionary::new();
            md.set("Type", Object::Name(b"XObject".to_vec()));
            md.set("Subtype", Object::Name(b"Image".to_vec()));
            md.set("Width", w as i64);
            md.set("Height", h as i64);
            md.set("BitsPerComponent", 8i64);
            md.set("ColorSpace", Object::Name(b"DeviceGray".to_vec()));
            let mut ms = Stream::new(md, alpha);
            let _ = ms.compress();
            mask = Some(ms);
        }
        image_stream(dict, &img, false)?
    };
    Ok((stream, mask, (w, h)))
}

fn x_len(m: &Mat) -> f64 {
    (m[0] * m[0] + m[1] * m[1]).sqrt()
}

/// Path of the PDFium object drawn by show `k` of a content stream.
fn objects_path(cp: &CPlan, k: usize) -> ObjPath {
    let mut p = cp.path.clone();
    p.push(cp.scan.shows[k].object);
    p
}

/// The operator's original text (for a replacement that changes nothing).
fn op_text(cp: &CPlan, sh: &Show) -> String {
    let op = &cp.scan.ops[sh.op];
    String::from_utf8_lossy(&cp.content[op.start..op.end]).into_owned()
}

/// A displacement on the displayed page (points, y down) in page space.
fn display_to_page_vec(tf: &PageTransform, dx: f64, dy: f64) -> (f64, f64) {
    let o = tf.to_user(0.0, 0.0);
    let p = tf.to_user(dx as f32, dy as f32);
    ((p.0 - o.0) as f64, (p.1 - o.1) as f64)
}

/// Text space advance of tokens in the operator's text state, when the widths are known.
fn tokens_advance(ctx: &Ctx, meta: &FontMeta, sh: &Show, toks: &[Tok], chars: &[char]) -> f64 {
    let mut adv = 0.0;
    let mut k = 0;
    for t in toks {
        match t {
            Tok::Code(c) => {
                let em = if meta.has_widths {
                    *meta.widths.get(&fonts::code_value(c)).unwrap_or(&meta.missing)
                } else {
                    chars.get(k).and_then(|ch| ctx.lib.advance(&meta.family, meta.bold, meta.italic, *ch)).unwrap_or(0.5) as f64
                };
                let ws = if c.as_slice() == [32] { sh.tw } else { 0.0 };
                adv += (em * sh.size + sh.tc + ws) * sh.th;
                k += 1;
            }
            Tok::Kern(n) => adv -= n / 1000.0 * sh.size * sh.th,
            Tok::Rise(_) => {}
        }
    }
    adv
}

/// Horizontal scaling that keeps `new_adv` within `old_adv`, when asked to fit.
fn fit_scale(fit: bool, th: f64, old_adv: f64, new_adv: f64) -> Option<f64> {
    if !fit || old_adv <= 0.0 || new_adv <= old_adv * 1.01 {
        return None;
    }
    Some(th * (old_adv / new_adv).max(0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_order_for_mixed_directions() {
        // Left-to-right paragraph with two Hebrew words in the middle.
        assert_eq!(visual_order_of(&[Some(false), Some(true), Some(true), Some(false)], false), vec![0, 2, 1, 3]);
        // Right-to-left paragraph with an English phrase and a number.
        assert_eq!(visual_order_of(&[Some(true), Some(false), Some(false), Some(true)], true), vec![3, 1, 2, 0]);
        assert_eq!(visual_order_of(&[Some(true), None, Some(true)], true), vec![2, 1, 0]);
    }

    #[test]
    fn finds_words_and_cases() {
        let o = FindOptions::default();
        assert_eq!(find_in("Hello hello", "hello", o), vec![(0, 5), (6, 11)]);
        assert_eq!(find_in("Hello hello", "hello", FindOptions { match_case: true, ..o }), vec![(6, 11)]);
        assert_eq!(find_in("cat concat cat", "cat", FindOptions { whole_word: true, ..o }), vec![(0, 3), (11, 14)]);
    }
}
