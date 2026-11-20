//! # pdf2word-core
//!
//! Converts PDF documents to Word (DOCX) files while preserving layout and
//! formatting:
//!
//! * text with fonts, sizes, bold/italic, colours, underline/strike-through,
//!   highlights, super/subscripts and hyperlinks,
//! * paragraphs with alignment, indents, first-line/hanging indents, tab
//!   stops and the original vertical spacing,
//! * multi-column layouts, ruled and whitespace-aligned tables (with merged
//!   cells, borders and shading), lists, separators,
//! * raster images and rasterised vector figures,
//! * scanned pages through Tesseract OCR.
//!
//! PDF parsing and rendering are done by [PDFium](https://pdfium.googlesource.com/pdfium/)
//! through `pdfium-render`, which loads `libpdfium` dynamically at runtime.

// Geometry code indexes several parallel arrays; explicit index loops read better there.
#![allow(clippy::needless_range_loop)]

pub mod docx;
pub mod embed;
pub mod extract;
pub mod fonts;
pub mod layout;
pub mod math;
pub mod metrics;
pub mod model;
pub mod ocr;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use pdfium_render::prelude::*;

pub use docx::{DocMeta, LayoutMode};
pub use extract::ExtractOptions;
pub use layout::LayoutOptions;
pub use ocr::OcrSettings;

/// When to run OCR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OcrMode {
    /// OCR pages that have no usable text layer (scans).
    #[default]
    Auto,
    /// OCR every page, ignoring the text layer.
    Always,
    /// Never OCR.
    Never,
}

impl std::str::FromStr for OcrMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(OcrMode::Auto),
            "always" | "force" | "on" => Ok(OcrMode::Always),
            "never" | "off" | "none" => Ok(OcrMode::Never),
            _ => bail!("invalid OCR mode '{s}' (expected auto, always or never)"),
        }
    }
}

impl std::str::FromStr for LayoutMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "flow" | "editable" => Ok(LayoutMode::Flow),
            "exact" | "fixed" | "positioned" => Ok(LayoutMode::Exact),
            _ => bail!("invalid layout mode '{s}' (expected flow or exact)"),
        }
    }
}

/// All conversion settings.
#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub layout_mode: LayoutMode,
    pub ocr_mode: OcrMode,
    pub ocr: OcrSettings,
    /// Rendering resolution used for OCR.
    pub ocr_dpi: f32,
    /// 1-based page selection such as `"1-3,7,10-"`; None = all pages.
    pub pages: Option<String>,
    pub password: Option<String>,
    pub extract: ExtractOptions,
    pub layout: LayoutOptions,
    pub detect_headings: bool,
    /// Adjust character spacing so text keeps its PDF width with Word's fonts.
    pub compensate_widths: bool,
    /// Embed the PDF's TrueType fonts into the DOCX when their licence allows it.
    pub embed_fonts: bool,
    /// Explicit path to the PDFium library (file or directory).
    pub pdfium_path: Option<PathBuf>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            layout_mode: LayoutMode::Flow,
            ocr_mode: OcrMode::Auto,
            ocr: OcrSettings::default(),
            ocr_dpi: 300.0,
            pages: None,
            password: None,
            extract: ExtractOptions::default(),
            layout: LayoutOptions::default(),
            detect_headings: true,
            compensate_widths: true,
            embed_fonts: true,
            pdfium_path: None,
        }
    }
}

/// Progress notifications emitted during conversion.
#[derive(Clone, Debug)]
pub enum Progress {
    Opened { pages: usize },
    PageStarted { page: usize, done: usize, total: usize },
    PageOcr { page: usize },
    PageDone { page: usize, done: usize, total: usize, ocr: bool },
    Writing,
    Finished { output: PathBuf },
    Warning(String),
}

/// Summary of a finished conversion.
#[derive(Clone, Debug, Default)]
pub struct ConvertReport {
    pub pages: usize,
    pub ocr_pages: Vec<usize>,
    pub images: usize,
    pub tables: usize,
    pub paragraphs: usize,
    pub warnings: Vec<String>,
    /// Font families embedded into the DOCX.
    pub embedded_fonts: Vec<String>,
}

/// Cooperative cancellation flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Locates and binds the PDFium library.
///
/// Search order: explicit path, `PDFIUM_DYNAMIC_LIB_PATH`, the executable's
/// directory (and `../lib`), the current directory, then the system library path.
pub fn load_pdfium(explicit: Option<&Path>) -> anyhow::Result<Pdfium> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let push = |p: PathBuf, c: &mut Vec<PathBuf>| {
        if p.is_dir() {
            c.push(Pdfium::pdfium_platform_library_name_at_path(&p));
        } else {
            c.push(p);
        }
    };
    if let Some(p) = explicit {
        push(p.to_path_buf(), &mut candidates);
    }
    if let Ok(p) = std::env::var("PDFIUM_DYNAMIC_LIB_PATH") {
        push(PathBuf::from(p), &mut candidates);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            push(dir.to_path_buf(), &mut candidates);
            push(dir.join("lib"), &mut candidates);
            push(dir.join("../lib"), &mut candidates);
        }
    }
    push(PathBuf::from("."), &mut candidates);

    let mut errors = Vec::new();
    for c in &candidates {
        if !c.exists() {
            continue;
        }
        match Pdfium::bind_to_library(c) {
            Ok(b) => return Ok(Pdfium::new(b)),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => return Ok(Pdfium::default()),
            Err(e) => errors.push(format!("{}: {e}", c.display())),
        }
    }
    match Pdfium::bind_to_system_library() {
        Ok(b) => Ok(Pdfium::new(b)),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Ok(Pdfium::default()),
        Err(e) => {
            errors.push(format!("system library: {e}"));
            Err(anyhow!(
                "could not load the PDFium library ({}). Download it from https://github.com/bblanchon/pdfium-binaries \
                 and place it next to the executable or set PDFIUM_DYNAMIC_LIB_PATH",
                errors.join("; ")
            ))
        }
    }
}

/// Parses a 1-based page selection ("1-3,5,9-") into 0-based indices.
pub fn parse_page_range(spec: &str, total: usize) -> anyhow::Result<Vec<usize>> {
    let mut pages = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => {
                let a = if a.trim().is_empty() { 1 } else { a.trim().parse::<usize>()? };
                let b = if b.trim().is_empty() { total } else { b.trim().parse::<usize>()? };
                (a, b)
            }
            None => {
                let n = part.parse::<usize>().with_context(|| format!("invalid page '{part}'"))?;
                (n, n)
            }
        };
        if a == 0 || b == 0 || a > b {
            bail!("invalid page range '{part}'");
        }
        for p in a..=b.min(total) {
            if !pages.contains(&(p - 1)) {
                pages.push(p - 1);
            }
        }
    }
    if pages.is_empty() {
        bail!("page selection '{spec}' selects no pages (document has {total})");
    }
    Ok(pages)
}

fn count_blocks(blocks: &[model::Block], r: &mut ConvertReport) {
    for b in blocks {
        match b {
            model::Block::Paragraph(_) => r.paragraphs += 1,
            model::Block::Picture(_) => r.images += 1,
            model::Block::Table(t) => {
                if !t.layout_only {
                    r.tables += 1;
                }
                for row in &t.rows {
                    for c in &row.cells {
                        count_blocks(&c.blocks, r);
                    }
                }
            }
            model::Block::Rule(_) => {}
        }
    }
}

/// Converts `input` (PDF) to `output` (DOCX).
pub fn convert(
    input: &Path,
    output: &Path,
    opts: &ConvertOptions,
    cancel: Option<&CancelToken>,
    progress: &mut dyn FnMut(Progress),
) -> anyhow::Result<ConvertReport> {
    let pdfium = load_pdfium(opts.pdfium_path.as_deref())?;
    let document = pdfium
        .load_pdf_from_file(input, opts.password.as_deref())
        .map_err(|e| match e {
            PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
                anyhow!("the PDF is password protected (supply the password)")
            }
            e => anyhow!("could not open {}: {e}", input.display()),
        })?;
    let total = document.pages().len() as usize;
    progress(Progress::Opened { pages: total });

    let selection: Vec<usize> = match &opts.pages {
        Some(spec) => parse_page_range(spec, total)?,
        None => (0..total).collect(),
    };

    let mut report = ConvertReport::default();
    let mut ocr_engine: Option<ocr::OcrEngine> = None;
    let mut ocr_failed = false;
    let mut layouts = Vec::with_capacity(selection.len());

    for (done, &index) in selection.iter().enumerate() {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            bail!("conversion cancelled");
        }
        progress(Progress::PageStarted { page: index + 1, done, total: selection.len() });
        let page = document.pages().get(index as PdfPageIndex)?;
        let (mut raw, stats) = extract::extract_page(&document, &page, index, &opts.extract)
            .with_context(|| format!("page {}", index + 1))?;

        // A scanned page with an invisible OCR text layer: drop the scan image, keep the text.
        if stats.invisible_chars > stats.visible_chars && stats.invisible_chars > 0 {
            let area = raw.width * raw.height;
            raw.images.retain(|im| im.bbox.area() < 0.5 * area);
        }

        let want_ocr = match opts.ocr_mode {
            OcrMode::Never => false,
            OcrMode::Always => true,
            OcrMode::Auto => stats.looks_scanned(),
        };
        let mut did_ocr = false;
        if want_ocr && !ocr_failed {
            if ocr_engine.is_none() {
                match ocr::OcrEngine::new(opts.ocr.clone()) {
                    Ok(e) => ocr_engine = Some(e),
                    Err(e) => {
                        ocr_failed = true;
                        let msg = format!("OCR unavailable: {e}");
                        progress(Progress::Warning(msg.clone()));
                        report.warnings.push(msg);
                    }
                }
            }
            if let Some(engine) = ocr_engine.as_mut() {
                progress(Progress::PageOcr { page: index + 1 });
                let img = extract::render_page(&page, opts.ocr_dpi)?;
                match engine.recognize(&img, opts.ocr_dpi) {
                    Ok(words) => {
                        let settings = engine.settings().clone();
                        ocr::words_to_glyphs(&mut raw, &words, &img, &settings);
                        raw.equations.clear();
                        // Table rulings and underlines of a scan only exist as pixels.
                        let scanned = raw.rules.is_empty();
                        if scanned {
                            raw.rules = ocr::detect_rules(&img, opts.ocr_dpi, &words);
                        }
                        // The scan itself is replaced by the recognised text.
                        let area = raw.width * raw.height;
                        raw.images.retain(|im| im.bbox.area() < 0.5 * area);
                        // Pictures inside the scan are cropped out of the rendering.
                        if opts.extract.extract_images && scanned {
                            let (iw, ih) = (img.width() as f32, img.height() as f32);
                            for r in ocr::detect_figures(&img, opts.ocr_dpi, &words, &raw.rules) {
                                if raw.images.iter().any(|im| im.bbox.intersects(&r)) {
                                    continue;
                                }
                                let (sx, sy) = (iw / raw.width, ih / raw.height);
                                let crop = img.crop_imm(
                                    (r.x0 * sx) as u32,
                                    (r.y0 * sy) as u32,
                                    ((r.width() * sx) as u32).max(1),
                                    ((r.height() * sy) as u32).max(1),
                                );
                                if let Some(data) = extract::encode_image(&crop, opts.extract.max_image_px) {
                                    raw.images.push(model::RawImage { bbox: r, data, behind_text: false });
                                }
                            }
                        }
                        did_ocr = true;
                        report.ocr_pages.push(index + 1);
                    }
                    Err(e) => {
                        let msg = format!("page {}: OCR failed: {e}", index + 1);
                        progress(Progress::Warning(msg.clone()));
                        report.warnings.push(msg);
                    }
                }
            }
        }

        let layout = layout::analyse_page(&raw, &opts.layout);
        count_blocks(&layout.blocks, &mut report);
        report.images += layout.floating.len();
        layouts.push(layout);
        progress(Progress::PageDone { page: index + 1, done: done + 1, total: selection.len(), ocr: did_ocr });
    }

    if opts.detect_headings {
        layout::detect_headings(&mut layouts);
    }
    let mut embedded = Vec::new();
    if opts.embed_fonts {
        match embed::read_pdf_fonts(input, opts.password.as_deref()) {
            Ok(programs) => embedded = embed::select(&layouts, &programs),
            Err(e) => log::debug!("font embedding skipped: {e}"),
        }
    }
    if opts.compensate_widths {
        let mut library = metrics::FontLibrary::default();
        for f in &embedded {
            if let Some(data) = metrics::FontData::new(f.data.clone(), 0) {
                library.register(&f.family, f.bold, f.italic, std::sync::Arc::new(data));
            }
        }
        metrics::compensate(&mut layouts, &library);
    }
    report.embedded_fonts = embedded.iter().map(|f| f.family.clone()).collect();
    report.embedded_fonts.dedup();

    progress(Progress::Writing);
    let mut meta = document_meta(&document, input);
    meta.fonts = embedded;
    let tmp = output.with_extension("docx.part");
    {
        let file = std::fs::File::create(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
        let mut buf = std::io::BufWriter::new(file);
        docx::write_docx(&mut buf, &layouts, &meta, opts.layout_mode)?;
        use std::io::Write;
        buf.flush()?;
    }
    std::fs::rename(&tmp, output).with_context(|| format!("cannot write {}", output.display()))?;
    report.pages = layouts.len();
    progress(Progress::Finished { output: output.to_path_buf() });
    Ok(report)
}

fn document_meta(document: &PdfDocument, input: &Path) -> DocMeta {
    let md = document.metadata();
    let get = |t: PdfDocumentMetadataTagType| {
        md.get(t).map(|v| v.value().trim().to_string()).filter(|v| !v.is_empty())
    };
    DocMeta {
        title: get(PdfDocumentMetadataTagType::Title)
            .or_else(|| input.file_stem().map(|s| s.to_string_lossy().into_owned())),
        author: get(PdfDocumentMetadataTagType::Author),
        subject: get(PdfDocumentMetadataTagType::Subject),
        keywords: get(PdfDocumentMetadataTagType::Keywords),
        fonts: Vec::new(),
    }
}

/// Basic information about a PDF (used by front-ends).
#[derive(Clone, Debug)]
pub struct PdfInfo {
    pub pages: usize,
    pub title: Option<String>,
    /// Number of pages (among the first few) that look scanned.
    pub scanned_pages_sampled: usize,
    pub sampled: usize,
}

/// Opens a PDF and returns basic information.
pub fn inspect(input: &Path, password: Option<&str>, pdfium_path: Option<&Path>) -> anyhow::Result<PdfInfo> {
    let pdfium = load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let pages = document.pages().len() as usize;
    let sampled = pages.min(3);
    let mut scanned = 0;
    let opts = ExtractOptions { extract_images: false, rasterize_vector_graphics: false, ..Default::default() };
    for i in 0..sampled {
        let page = document.pages().get(i as PdfPageIndex)?;
        if let Ok((_, stats)) = extract::extract_page(&document, &page, i, &opts) {
            if stats.looks_scanned() {
                scanned += 1;
            }
        }
    }
    Ok(PdfInfo { pages, title: document_meta(&document, input).title, scanned_pages_sampled: scanned, sampled })
}

/// Renders a page to a PNG file (for previews). `page` is 0-based.
pub fn render_page_png(
    input: &Path,
    password: Option<&str>,
    page: usize,
    width_px: u32,
    out: &Path,
    pdfium_path: Option<&Path>,
) -> anyhow::Result<()> {
    let pdfium = load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let p = document.pages().get(page as PdfPageIndex)?;
    let bitmap = p.render_with_config(
        &PdfRenderConfig::new()
            .set_target_width(width_px as i32)
            .render_form_data(true)
            .set_clear_color(PdfColor::WHITE),
    )?;
    bitmap.as_image()?.to_rgb8().save(out)?;
    Ok(())
}

/// Version string of the OCR engine (or "unavailable").
pub fn ocr_version() -> String {
    ocr::OcrEngine::version()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_ranges() {
        assert_eq!(parse_page_range("1-3,5", 10).unwrap(), vec![0, 1, 2, 4]);
        assert_eq!(parse_page_range("8-", 10).unwrap(), vec![7, 8, 9]);
        assert_eq!(parse_page_range("-2", 10).unwrap(), vec![0, 1]);
        assert!(parse_page_range("0", 10).is_err());
        assert!(parse_page_range("20", 10).is_err());
    }

    #[test]
    fn modes_parse() {
        assert_eq!("exact".parse::<LayoutMode>().unwrap(), LayoutMode::Exact);
        assert_eq!("never".parse::<OcrMode>().unwrap(), OcrMode::Never);
        assert!("bogus".parse::<OcrMode>().is_err());
    }
}
