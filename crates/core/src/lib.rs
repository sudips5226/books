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

pub mod annots;
pub mod docx;
pub mod edit;
pub mod embed;
pub mod extract;
pub mod fonts;
pub mod layout;
pub mod math;
pub mod metrics;
pub mod navigate;
pub mod model;
pub mod ocr;
pub mod pages;
pub mod reflow;
pub mod scan;
pub mod structure;

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
            "continuous" | "reflow" => Ok(LayoutMode::Continuous),
            _ => bail!("invalid layout mode '{s}' (expected flow, continuous or exact)"),
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
    /// Move repeated page tops/bottoms into Word headers and footers.
    pub headers_footers: bool,
    /// Turn typed list markers into Word lists.
    pub word_lists: bool,
    /// Turn notes at the page bottom into Word footnotes.
    pub footnotes: bool,
    /// Use named paragraph styles (Normal, Heading n, Caption …).
    pub paragraph_styles: bool,
    /// Heading levels and bookmarks from the PDF outline, internal links and
    /// the table of contents as Word bookmarks, hyperlinks and a TOC field.
    pub navigation: bool,
    /// Annotations become Word comments; form fields become content controls.
    pub annotations: bool,
    /// Turn scans upright and straighten skewed scans before OCR.
    pub straighten_scans: bool,
    /// Recognise text in pictures and use it as their alternative text.
    pub picture_text: bool,
    /// Worker threads for OCR and layout analysis (None = number of CPUs, at most 8).
    pub threads: Option<usize>,
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
            headers_footers: true,
            word_lists: true,
            footnotes: true,
            paragraph_styles: true,
            navigation: true,
            annotations: true,
            straighten_scans: true,
            picture_text: true,
            threads: None,
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
    /// Language used for OCR, if any page was recognised.
    pub ocr_language: Option<String>,
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
            push(dir.join("../Frameworks"), &mut candidates);
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
    let mut slots: Vec<Option<model::RawPage>> = (0..selection.len()).map(|_| None).collect();
    let workers = opts.threads.unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2)).clamp(1, 8);
    let mut ocr_settings = opts.ocr.clone();
    let mut ocr_unavailable = false;
    let warn = |report: &mut ConvertReport, progress: &mut dyn FnMut(Progress), msg: String| {
        progress(Progress::Warning(msg.clone()));
        report.warnings.push(msg);
    };

    // Pages are extracted (and scans rendered) here, PDFium being single-threaded;
    // OCR runs on worker threads fed through a bounded queue, which caps the
    // number of page images held in memory.
    struct Job {
        slot: usize,
        index: usize,
        raw: model::RawPage,
        img: image::DynamicImage,
    }
    struct Done {
        slot: usize,
        index: usize,
        raw: model::RawPage,
        result: Result<ocr::ScanReport, String>,
        engine_failed: bool,
    }
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let (job_tx, job_rx) = std::sync::mpsc::sync_channel::<Job>(workers);
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Done>();
        let mut spawned = false;
        let mut finished = 0usize;
        let handle = |d: Done, slots: &mut Vec<Option<model::RawPage>>, report: &mut ConvertReport, progress: &mut dyn FnMut(Progress), unavailable: &mut bool, finished: &mut usize| {
            match &d.result {
                Ok(r) => {
                    report.ocr_pages.push(d.index + 1);
                    if r.quarter_turns != 0 {
                        log::info!("page {}: turned upright ({}°)", d.index + 1, r.quarter_turns as u32 * 90);
                    }
                }
                Err(e) if d.engine_failed => {
                    if !*unavailable {
                        warn(report, progress, format!("OCR unavailable: {e}"));
                    }
                    *unavailable = true;
                }
                Err(e) => warn(report, progress, format!("page {}: OCR failed: {e}", d.index + 1)),
            }
            let ocr = d.result.is_ok();
            slots[d.slot] = Some(d.raw);
            *finished += 1;
            progress(Progress::PageDone { page: d.index + 1, done: *finished, total: selection.len(), ocr });
        };
        for (done, &index) in selection.iter().enumerate() {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                bail!("conversion cancelled");
            }
            progress(Progress::PageStarted { page: index + 1, done, total: selection.len() });
            let page = document.pages().get(index as PdfPageIndex)?;
            let (mut raw, stats) = extract::extract_page(&document, &page, index, &opts.extract).with_context(|| format!("page {}", index + 1))?;

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
            if want_ocr && !ocr_unavailable {
                let img = extract::render_page(&page, opts.ocr_dpi)?;
                if !spawned {
                    // Language: as given, or detected from the document.
                    if ocr_settings.language.eq_ignore_ascii_case("auto") {
                        let lang = choose_language(&document, &selection, &img, opts, &mut |m| warn(&mut report, progress, m));
                        log::info!("OCR language: {lang}");
                        ocr_settings.language = lang;
                    }
                    report.ocr_language = Some(ocr_settings.language.clone());
                    for _ in 0..workers {
                        let rx = job_rx.clone();
                        let tx = done_tx.clone();
                        let settings = ocr_settings.clone();
                        let (dpi, eopts, straighten) = (opts.ocr_dpi, &opts.extract, opts.straighten_scans);
                        scope.spawn(move || {
                            let mut engine: Option<Result<ocr::OcrEngine, String>> = None;
                            loop {
                                let job = rx.lock().map_err(|_| ()).and_then(|r| r.recv().map_err(|_| ()));
                                let Ok(mut job) = job else { break };
                                let e = engine.get_or_insert_with(|| ocr::OcrEngine::new(settings.clone()).map_err(|e| e.to_string()));
                                let (result, engine_failed) = match e {
                                    Ok(e) => (ocr::process_scan(e, &mut job.raw, job.img, dpi, eopts, straighten).map_err(|e| e.to_string()), false),
                                    Err(msg) => (Err(msg.clone()), true),
                                };
                                if tx.send(Done { slot: job.slot, index: job.index, raw: job.raw, result, engine_failed }).is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    spawned = true;
                }
                progress(Progress::PageOcr { page: index + 1 });
                // Blocks while the workers are busy: at most `workers` images wait.
                if job_tx.send(Job { slot: done, index, raw, img }).is_err() {
                    bail!("OCR workers stopped");
                }
            } else {
                slots[done] = Some(raw);
                finished += 1;
                progress(Progress::PageDone { page: index + 1, done: finished, total: selection.len(), ocr: false });
            }
            while let Ok(d) = done_rx.try_recv() {
                handle(d, &mut slots, &mut report, progress, &mut ocr_unavailable, &mut finished);
            }
        }
        drop(job_tx);
        drop(done_tx);
        for d in done_rx {
            handle(d, &mut slots, &mut report, progress, &mut ocr_unavailable, &mut finished);
        }
        Ok(())
    })?;
    let mut raws: Vec<model::RawPage> = slots.into_iter().flatten().collect();
    report.ocr_pages.sort_unstable();

    // Text in pictures becomes their alternative text.
    if opts.ocr_mode != OcrMode::Never && opts.picture_text && !ocr_unavailable {
        let jobs: Vec<(usize, usize)> = raws
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.ocr)
            .flat_map(|(pi, r)| r.images.iter().enumerate().filter(|(_, im)| !im.overlay && im.vector.is_none() && im.bbox.width() >= 40.0 && im.bbox.height() >= 14.0).map(move |(ii, _)| (pi, ii)))
            .take(60)
            .collect();
        if !jobs.is_empty() {
            if ocr_settings.language.eq_ignore_ascii_case("auto") {
                let text: String = raws.iter().flat_map(|r| r.glyphs.iter().map(|g| g.ch)).take(20000).collect();
                let installed = scan::available_languages(ocr_settings.tessdata.as_deref());
                ocr_settings.language = scan::detect_language(&text).filter(|l| installed.contains(l)).unwrap_or_else(|| default_language(&installed));
            }
            let datas: Vec<model::ImageData> = jobs.iter().map(|&(pi, ii)| raws[pi].images[ii].data.clone()).collect();
            let texts = par_map(&datas, workers, || ocr::OcrEngine::new(ocr_settings.clone()).ok(), |engine, data| engine.as_mut().and_then(|e| ocr::picture_text(e, data)));
            for (&(pi, ii), t) in jobs.iter().zip(texts) {
                raws[pi].images[ii].alt = t;
            }
        }
    }

    // Repeated page tops and bottoms become the Word header and footer.
    let mut structure = model::DocStructure::default();
    // Page counts are only meaningful when the whole document is converted.
    let total_pages = if selection.len() == total { total } else { 0 };
    let mut hf_layouts: Vec<(usize, f32, f32)> = Vec::new(); // (which, margin_left, margin_right)
    let mut number_offset = 0i64;
    if opts.headers_footers {
        let hf = structure::extract_headers_footers(&mut raws);
        number_offset = hf.number_offset;
        let mut build = |(raw, src): (model::RawPage, usize), which: usize| {
            let footer = which % 2 == 1;
            let l = layout::analyse_page(&raw, &opts.layout);
            let mut blocks = l.blocks;
            let printed = (src as i64 + 1 + hf.number_offset).max(1) as usize;
            structure::insert_page_fields(&mut blocks, printed, total_pages);
            let boxes = || raw.glyphs.iter().map(|g| g.bbox).chain(raw.rules.iter().map(|r| r.bbox)).chain(raw.images.iter().map(|i| i.bbox));
            let content_top = boxes().map(|b| b.y0).fold(f32::MAX, f32::min);
            let content_bottom = boxes().map(|b| b.y1).fold(0.0, f32::max);
            structure::unwrap_single_cell_tables(&mut blocks);
            let height = (content_bottom - content_top).max(0.0).max(structure::blocks_height(&blocks));
            let distance = if footer { (raw.height - content_bottom).max(0.0) } else { l.margin_top };
            hf_layouts.push((which, l.margin_left, l.margin_right));
            model::HeaderFooter { blocks, distance, height }
        };
        structure.header = hf.header.map(|h| build(h, 0));
        structure.footer = hf.footer.map(|f| build(f, 1));
        structure.header_even = hf.header_even.map(|h| build(h, 2));
        structure.footer_even = hf.footer_even.map(|f| build(f, 3));
        structure.pages_without_header = hf.pages_without_header.clone();
        structure.page_numbers = hf.page_numbers.clone();
        structure.pages_without_footer = hf.pages_without_footer.clone();
        structure.blank_first_header = hf.blank_first_header;
        structure.blank_first_footer = hf.blank_first_footer;
    }

    let mut layouts: Vec<model::PageLayout> = par_map(&raws, workers, || (), |_, raw| layout::analyse_page(raw, &opts.layout));
    // Review comments and form fields of each page (by PDF page index).
    let mut comments: Vec<(usize, model::RawComment)> = Vec::new();
    let mut fields: std::collections::HashMap<usize, Vec<model::FormControl>> = Default::default();
    for raw in &mut raws {
        comments.extend(std::mem::take(&mut raw.comments).into_iter().map(|c| (raw.index, c)));
        if !raw.fields.is_empty() {
            fields.insert(raw.index, std::mem::take(&mut raw.fields));
        }
    }
    drop(raws);
    for l in &layouts {
        count_blocks(&l.blocks, &mut report);
        report.images += l.floating.len();
    }
    // Header/footer indents are relative to the body margins in Word.
    if !layouts.is_empty() {
        let median = |mut v: Vec<f32>| {
            v.sort_by(f32::total_cmp);
            v[v.len() / 2]
        };
        let ml = median(layouts.iter().map(|l| l.margin_left).collect());
        let mr = median(layouts.iter().map(|l| l.margin_right).collect());
        for (which, l, r) in hf_layouts {
            let target = match which {
                0 => structure.header.as_mut(),
                1 => structure.footer.as_mut(),
                2 => structure.header_even.as_mut(),
                _ => structure.footer_even.as_mut(),
            };
            if let Some(hf) = target {
                structure::shift_blocks(&mut hf.blocks, l - ml, r - mr);
            }
        }
    }

    if opts.detect_headings {
        layout::detect_headings(&mut layouts);
    }
    if opts.navigation {
        let outline = annots::outline(&document);
        if !outline.is_empty() {
            let n = navigate::apply_outline(&mut layouts, &outline);
            log::debug!("outline: {n} of {} entries matched", outline.len());
        }
    }
    structure::mark_header_rows(&mut layouts);
    navigate::insert_controls(&mut layouts, &fields);
    if opts.annotations {
        navigate::attach_comments(&mut layouts, comments, &mut structure);
    }
    if opts.word_lists {
        structure.lists = structure::detect_lists(&mut layouts);
    }
    // Footnotes cannot live in the positioned frames of exact layout.
    if opts.footnotes && opts.layout_mode != LayoutMode::Exact {
        let body = body_font_size(&layouts);
        structure.footnotes = structure::detect_footnotes(&mut layouts, body);
    }
    let mut toc_styles = Vec::new();
    if opts.navigation {
        navigate::resolve_internal_links(&mut layouts, &mut structure);
        toc_styles = navigate::detect_toc(&mut layouts, opts.layout_mode != LayoutMode::Exact);
    } else {
        // Unresolved jumps inside the document are dropped.
        navigate::for_all_paragraphs(&mut layouts, &mut structure, &mut |p: &mut model::Paragraph| {
            for i in p.inlines.iter_mut() {
                if let model::Inline::Text { link, .. } = i {
                    if link.as_ref().is_some_and(|l| l.starts_with('#')) {
                        *link = None;
                    }
                }
            }
        });
    }
    if opts.paragraph_styles {
        structure.styles = structure::assign_styles(&mut layouts, &mut structure.footnotes);
        structure.styles.extend(toc_styles);
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
        for hf in [structure.header.as_mut(), structure.footer.as_mut(), structure.header_even.as_mut(), structure.footer_even.as_mut()].into_iter().flatten() {
            metrics::compensate_blocks(&mut hf.blocks, &library);
        }
        for f in &mut structure.footnotes {
            metrics::compensate_blocks(&mut f.blocks, &library);
        }
    }
    report.embedded_fonts = embedded.iter().map(|f| f.family.clone()).collect();
    report.embedded_fonts.dedup();

    progress(Progress::Writing);
    let mut meta = document_meta(&document, input);
    meta.fonts = embedded;
    meta.structure = structure;
    meta.first_page_number = (selection.first().map(|p| *p as i64 + 1).unwrap_or(1) + number_offset).max(1) as usize;
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

/// Maps `f` over `items` on up to `workers` threads, each with its own state
/// from `init` (e.g. an OCR engine); results keep the input order.
fn par_map<T: Sync, R: Send, S>(items: &[T], workers: usize, init: impl Fn() -> S + Sync, f: impl Fn(&mut S, &T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    if n <= 1 || workers <= 1 {
        let mut st = init();
        return items.iter().map(|it| f(&mut st, it)).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..n).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(n) {
            scope.spawn(|| {
                let mut st = init();
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let r = f(&mut st, &items[i]);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    results.into_inner().unwrap().into_iter().map(|r| r.expect("every item is processed")).collect()
}

fn default_language(installed: &[String]) -> String {
    if installed.is_empty() || installed.iter().any(|l| l == "eng") {
        "eng".into()
    } else {
        installed[0].clone()
    }
}

/// OCR language for `auto`: from the document's own text, or from a quick
/// recognition of the first scanned page, limited to installed languages.
fn choose_language(document: &PdfDocument, selection: &[usize], first_scan: &image::DynamicImage, opts: &ConvertOptions, warn: &mut dyn FnMut(String)) -> String {
    let installed = scan::available_languages(opts.ocr.tessdata.as_deref());
    let fallback = default_language(&installed);
    // Text layers of the converted pages.
    let mut text = String::new();
    for &i in selection.iter().take(20) {
        if let Ok(page) = document.pages().get(i as PdfPageIndex) {
            if let Ok(t) = page.text() {
                text.push_str(&t.all());
                text.push(' ');
            }
        }
        if text.len() > 20000 {
            break;
        }
    }
    let mut detected = if text.chars().filter(|c| c.is_alphabetic()).count() >= 200 { scan::detect_language(&text) } else { None };
    // Otherwise a first look at the scan.
    if detected.is_none() {
        let settings = ocr::OcrSettings { language: fallback.clone(), ..opts.ocr.clone() };
        if let Ok(mut engine) = ocr::OcrEngine::new(settings) {
            let k = (150.0 / opts.ocr_dpi).min(1.0);
            let (w, h) = (first_scan.width(), first_scan.height());
            let small = first_scan.resize(((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1), image::imageops::FilterType::Triangle);
            if let Ok(words) = engine.recognize(&small, opts.ocr_dpi * k) {
                let t: Vec<&str> = words.iter().filter(|w| w.confidence > 40.0).map(|w| w.text.as_str()).collect();
                detected = scan::detect_language(&t.join(" "));
            }
        }
    }
    match detected {
        Some(l) if installed.is_empty() || installed.contains(&l) => l,
        Some(l) => {
            warn(format!("the document seems to be in '{l}', but Tesseract has no data for it; using '{fallback}'"));
            fallback
        }
        None => fallback,
    }
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
        ..Default::default()
    }
}

/// The most common font size of the document's text (by characters).
fn body_font_size(pages: &[model::PageLayout]) -> f32 {
    let mut hist: std::collections::HashMap<i32, usize> = Default::default();
    let mut pages = pages.to_vec();
    for p in &mut pages {
        structure::for_each_paragraph(&mut p.blocks, &mut |par: &mut model::Paragraph| {
            for i in &par.inlines {
                if let model::Inline::Text { text, style, .. } = i {
                    *hist.entry((style.size * 2.0).round() as i32).or_default() += text.len();
                }
            }
        });
    }
    hist.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s as f32 / 2.0).unwrap_or(11.0)
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
/// A detected element of a page, for showing the structure over a preview.
#[derive(Clone, Debug)]
pub struct StructureBox {
    /// "heading", "paragraph", "list", "table", "column", "picture", "figure", "equation", "rule", "textbox", "comment", "field".
    pub kind: &'static str,
    /// Position as fractions of the page width and height (0..1).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Short description (heading level, table size …).
    pub label: String,
}

/// The structure the converter finds on one page (without OCR), for previews.
pub fn page_structure(input: &Path, password: Option<&str>, page: usize, pdfium_path: Option<&Path>) -> anyhow::Result<Vec<StructureBox>> {
    let pdfium = load_pdfium(pdfium_path)?;
    let document = pdfium.load_pdf_from_file(input, password).map_err(|e| anyhow!("could not open {}: {e}", input.display()))?;
    let p = document.pages().get(page as PdfPageIndex)?;
    let opts = ConvertOptions::default();
    let (raw, _) = extract::extract_page(&document, &p, page, &opts.extract)?;
    let mut layouts = vec![layout::analyse_page(&raw, &opts.layout)];
    layout::detect_headings(&mut layouts);
    structure::detect_lists(&mut layouts);
    let l = &layouts[0];
    let (pw, ph) = (l.width.max(1.0), l.height.max(1.0));
    let mut out = Vec::new();
    let mut push = |kind: &'static str, r: model::Rect, label: String| {
        if r.width() > 0.0 && r.height() > 0.0 {
            out.push(StructureBox { kind, x: r.x0 / pw, y: r.y0 / ph, w: r.width() / pw, h: r.height() / ph, label });
        }
    };
    fn walk(blocks: &[model::Block], push: &mut dyn FnMut(&'static str, model::Rect, String)) {
        for b in blocks {
            match b {
                model::Block::Paragraph(p) => {
                    let math = p.inlines.iter().any(|i| matches!(i, model::Inline::Math(_))) && p.plain_text().trim().len() < 3;
                    let (kind, label) = match (p.heading, p.list, math) {
                        (Some(h), _, _) => ("heading", format!("Heading {h}")),
                        (_, Some(l), _) => ("list", format!("List item (level {})", l.level + 1)),
                        (_, _, true) => ("equation", "Equation".to_string()),
                        _ => ("paragraph", format!("Paragraph, {} line{}", p.n_lines, if p.n_lines == 1 { "" } else { "s" })),
                    };
                    push(kind, p.bbox, label);
                }
                model::Block::Table(t) if t.layout_only => {
                    for r in &t.rows {
                        for c in &r.cells {
                            if let Some(bb) = blocks_bbox(&c.blocks) {
                                push("column", bb, "Column".into());
                            }
                            walk(&c.blocks, push);
                        }
                    }
                }
                model::Block::Table(t) => push("table", t.bbox, format!("Table, {} × {}", t.rows.len(), t.col_widths.len())),
                model::Block::Picture(pic) => push(if pic.vector.is_some() { "figure" } else { "picture" }, pic.bbox, if pic.vector.is_some() { "Figure (editable shapes)".into() } else { "Picture".into() }),
                model::Block::Rule(r) => push("rule", r.bbox.expand(1.0), "Rule".into()),
            }
        }
    }
    fn blocks_bbox(blocks: &[model::Block]) -> Option<model::Rect> {
        let rects: Vec<model::Rect> = blocks
            .iter()
            .map(|b| match b {
                model::Block::Paragraph(p) => p.bbox,
                model::Block::Table(t) => t.bbox,
                model::Block::Picture(p) => p.bbox,
                model::Block::Rule(r) => r.bbox,
            })
            .collect();
        model::Rect::union_all(rects.iter())
    }
    walk(&l.blocks, &mut push);
    for f in &l.floating {
        push("picture", f.bbox, if f.overlay { "Annotation".into() } else { "Floating picture".into() });
    }
    for tb in &l.text_boxes {
        let (w, h) = if (tb.rotation.rem_euclid(180.0) - 90.0).abs() < 2.0 { (tb.height, tb.width) } else { (tb.width, tb.height) };
        push("textbox", model::Rect::new(tb.cx - w / 2.0, tb.cy - h / 2.0, tb.cx + w / 2.0, tb.cy + h / 2.0), "Text box".into());
    }
    for c in &raw.comments {
        push("comment", c.bbox, format!("Comment: {}", c.text.chars().take(60).collect::<String>()));
    }
    Ok(out)
}

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
