//! The `Converter` QObject exposed to QML.
//!
//! All PDF work (inspection, preview rendering, conversion) runs on background
//! threads; results are queued back onto the Qt event loop with `CxxQtThread`.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QUrl};
use pdf2word_core::{CancelToken, ConvertOptions, LayoutMode, OcrMode, Progress};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, input_path, cxx_name = "inputPath")]
        #[qproperty(QString, output_path, cxx_name = "outputPath")]
        #[qproperty(i32, page_count, cxx_name = "pageCount")]
        #[qproperty(QString, document_info, cxx_name = "documentInfo")]
        #[qproperty(i32, preview_page, cxx_name = "previewPage")]
        #[qproperty(QUrl, preview_source, cxx_name = "previewSource")]
        #[qproperty(bool, busy)]
        #[qproperty(f64, progress)]
        #[qproperty(QString, status)]
        #[qproperty(QString, log)]
        #[qproperty(QString, last_output, cxx_name = "lastOutput")]
        #[qproperty(i32, layout_mode, cxx_name = "layoutMode")]
        #[qproperty(i32, ocr_mode, cxx_name = "ocrMode")]
        #[qproperty(QString, ocr_language, cxx_name = "ocrLanguage")]
        #[qproperty(i32, ocr_dpi, cxx_name = "ocrDpi")]
        #[qproperty(QString, page_range, cxx_name = "pageRange")]
        #[qproperty(QString, password)]
        #[qproperty(bool, extract_images, cxx_name = "extractImages")]
        #[qproperty(bool, vector_figures, cxx_name = "vectorFigures")]
        #[qproperty(bool, detect_tables, cxx_name = "detectTables")]
        #[qproperty(bool, detect_headings, cxx_name = "detectHeadings")]
        #[qproperty(bool, embed_fonts, cxx_name = "embedFonts")]
        #[qproperty(bool, keep_widths, cxx_name = "keepWidths")]
        #[qproperty(QString, ocr_engine, cxx_name = "ocrEngine")]
        type Converter = super::ConverterRust;
    }

    extern "RustQt" {
        /// Opens a PDF chosen in a file dialog or dropped on the window.
        #[qinvokable]
        #[cxx_name = "openPdf"]
        fn open_pdf(self: Pin<&mut Converter>, url: &QUrl);

        /// Sets the output file from a save dialog URL.
        #[qinvokable]
        #[cxx_name = "setOutputUrl"]
        fn set_output_url(self: Pin<&mut Converter>, url: &QUrl);

        /// Renders the given (1-based) page into the preview.
        #[qinvokable]
        #[cxx_name = "showPage"]
        fn show_page(self: Pin<&mut Converter>, page: i32);

        #[qinvokable]
        fn convert(self: Pin<&mut Converter>);

        #[qinvokable]
        fn cancel(self: Pin<&mut Converter>);

        /// URL of the last written document (for "Open").
        #[qinvokable]
        #[cxx_name = "outputUrl"]
        fn output_url(self: &Converter) -> QUrl;

        /// URL of the folder containing the last written document.
        #[qinvokable]
        #[cxx_name = "outputFolderUrl"]
        fn output_folder_url(self: &Converter) -> QUrl;

        /// Opens a PDF passed on the command line (file associations, "Open with…").
        #[qinvokable]
        #[cxx_name = "openFromArguments"]
        fn open_from_arguments(self: Pin<&mut Converter>);

        #[qsignal]
        fn finished(self: Pin<&mut Converter>, success: bool, message: QString);
    }

    impl cxx_qt::Threading for Converter {}
}

pub struct ConverterRust {
    input_path: QString,
    output_path: QString,
    page_count: i32,
    document_info: QString,
    preview_page: i32,
    preview_source: QUrl,
    busy: bool,
    progress: f64,
    status: QString,
    log: QString,
    last_output: QString,
    layout_mode: i32,
    ocr_mode: i32,
    ocr_language: QString,
    ocr_dpi: i32,
    page_range: QString,
    password: QString,
    extract_images: bool,
    vector_figures: bool,
    detect_tables: bool,
    detect_headings: bool,
    embed_fonts: bool,
    keep_widths: bool,
    ocr_engine: QString,

    cancel: Option<CancelToken>,
}

impl Default for ConverterRust {
    fn default() -> Self {
        Self {
            input_path: QString::default(),
            output_path: QString::default(),
            page_count: 0,
            document_info: QString::from("Drop a PDF here or click “Open PDF…”"),
            preview_page: 1,
            preview_source: QUrl::default(),
            busy: false,
            progress: 0.0,
            status: QString::from("Ready"),
            log: QString::default(),
            last_output: QString::default(),
            layout_mode: 0,
            ocr_mode: 0,
            ocr_language: QString::from("eng"),
            ocr_dpi: 300,
            page_range: QString::default(),
            password: QString::default(),
            extract_images: true,
            vector_figures: true,
            detect_tables: true,
            detect_headings: true,
            embed_fonts: true,
            keep_widths: true,
            ocr_engine: QString::from(&format!("Tesseract {}", pdf2word_core::ocr_version())),
            cancel: None,
        }
    }
}

static PREVIEW_COUNTER: AtomicU64 = AtomicU64::new(0);

fn opt_string(s: &QString) -> Option<String> {
    let s = s.to_string();
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn url_to_path(url: &QUrl) -> Option<PathBuf> {
    url.to_local_file().map(|s| PathBuf::from(s.to_string())).filter(|p| !p.as_os_str().is_empty())
}

fn path_to_url(path: &Path) -> QUrl {
    QUrl::from_local_file(&QString::from(&path.to_string_lossy().into_owned()))
}

fn default_output(input: &Path) -> PathBuf {
    input.with_extension("docx")
}

impl qobject::Converter {
    fn append_log(mut self: Pin<&mut Self>, line: &str) {
        let mut log = self.log().to_string();
        if !log.is_empty() {
            log.push('\n');
        }
        log.push_str(line);
        // Keep the log bounded.
        if log.len() > 64 * 1024 {
            let cut = log.len() - 48 * 1024;
            let cut = log[cut..].find('\n').map_or(cut, |i| cut + i + 1);
            log = log[cut..].to_string();
        }
        self.as_mut().set_log(QString::from(&log));
    }

    pub fn open_pdf(mut self: Pin<&mut Self>, url: &QUrl) {
        let Some(path) = url_to_path(url) else {
            self.as_mut().set_status(QString::from("Only local files can be opened"));
            return;
        };
        if self.busy {
            return;
        }
        self.as_mut().set_input_path(QString::from(&path.to_string_lossy().into_owned()));
        self.as_mut().set_output_path(QString::from(&default_output(&path).to_string_lossy().into_owned()));
        self.as_mut().set_page_count(0);
        self.as_mut().set_progress(0.0);
        self.as_mut().set_last_output(QString::default());
        self.as_mut().set_document_info(QString::from("Reading document…"));
        self.as_mut().set_status(QString::from("Ready"));

        let password = opt_string(self.password());
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = pdf2word_core::inspect(&path, password.as_deref(), None);
            let _ = qt_thread.queue(move |mut obj| match result {
                Ok(info) => {
                    let mut text = format!("{} page{}", info.pages, if info.pages == 1 { "" } else { "s" });
                    if let Some(t) = info.title.filter(|t| !t.is_empty()) {
                        text = format!("{t} — {text}");
                    }
                    if info.scanned_pages_sampled > 0 {
                        text.push_str(&format!(
                            " · scanned pages detected ({} of first {}) — OCR will be used",
                            info.scanned_pages_sampled, info.sampled
                        ));
                    }
                    obj.as_mut().set_page_count(info.pages as i32);
                    obj.as_mut().set_document_info(QString::from(&text));
                    obj.as_mut().show_page(1);
                }
                Err(e) => {
                    obj.as_mut().set_document_info(QString::from(&format!("Cannot open: {e:#}")));
                    obj.as_mut().set_preview_source(QUrl::default());
                }
            });
        });
    }

    pub fn open_from_arguments(self: Pin<&mut Self>) {
        let Some(arg) = std::env::args_os().skip(1).find(|a| !a.to_string_lossy().starts_with('-')) else {
            return;
        };
        let path = std::fs::canonicalize(&arg).unwrap_or_else(|_| PathBuf::from(&arg));
        let url = path_to_url(&path);
        self.open_pdf(&url);
    }

    pub fn set_output_url(mut self: Pin<&mut Self>, url: &QUrl) {
        if let Some(mut p) = url_to_path(url) {
            if p.extension().is_none_or(|e| !e.eq_ignore_ascii_case("docx")) {
                p.set_extension("docx");
            }
            self.as_mut().set_output_path(QString::from(&p.to_string_lossy().into_owned()));
        }
    }

    pub fn show_page(mut self: Pin<&mut Self>, page: i32) {
        let count = *self.page_count();
        if count <= 0 {
            return;
        }
        let page = page.clamp(1, count);
        self.as_mut().set_preview_page(page);
        let input = PathBuf::from(self.input_path().to_string());
        let password = opt_string(self.password());
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let n = PREVIEW_COUNTER.fetch_add(1, Ordering::SeqCst);
            let out = std::env::temp_dir().join(format!("pdf2word-preview-{}-{n}.png", std::process::id()));
            let result = pdf2word_core::render_page_png(&input, password.as_deref(), (page - 1) as usize, 900, &out, None);
            let _ = qt_thread.queue(move |mut obj| {
                // Ignore stale renders.
                if *obj.preview_page() != page {
                    let _ = std::fs::remove_file(&out);
                    return;
                }
                match result {
                    Ok(()) => {
                        if let Some(old) = url_to_path(obj.preview_source()) {
                            let _ = std::fs::remove_file(old);
                        }
                        obj.as_mut().set_preview_source(path_to_url(&out));
                    }
                    Err(e) => obj.as_mut().set_status(QString::from(&format!("Preview failed: {e:#}"))),
                }
            });
        });
    }

    fn options(&self) -> ConvertOptions {
        let mut o = ConvertOptions {
            layout_mode: if *self.layout_mode() == 1 { LayoutMode::Exact } else { LayoutMode::Flow },
            ocr_mode: match *self.ocr_mode() {
                1 => OcrMode::Always,
                2 => OcrMode::Never,
                _ => OcrMode::Auto,
            },
            ocr_dpi: (*self.ocr_dpi()).clamp(100, 600) as f32,
            pages: opt_string(self.page_range()),
            password: opt_string(self.password()),
            detect_headings: *self.detect_headings(),
            embed_fonts: *self.embed_fonts(),
            compensate_widths: *self.keep_widths(),
            ..Default::default()
        };
        o.ocr.language = opt_string(self.ocr_language()).unwrap_or_else(|| "eng".into());
        o.extract.extract_images = *self.extract_images();
        o.extract.rasterize_vector_graphics = *self.vector_figures() && *self.extract_images();
        o.layout.detect_tables = *self.detect_tables();
        o
    }

    pub fn convert(mut self: Pin<&mut Self>) {
        if self.busy {
            return;
        }
        let input = PathBuf::from(self.input_path().to_string());
        if !input.is_file() {
            self.as_mut().set_status(QString::from("Choose a PDF file first"));
            return;
        }
        let output = match opt_string(self.output_path()) {
            Some(o) => PathBuf::from(o),
            None => default_output(&input),
        };
        let opts = self.options();
        let token = CancelToken::new();
        self.as_mut().rust_mut().cancel = Some(token.clone());
        self.as_mut().set_busy(true);
        self.as_mut().set_progress(0.0);
        self.as_mut().set_last_output(QString::default());
        self.as_mut().set_status(QString::from("Converting…"));
        self.as_mut().append_log(&format!("▶ {} → {}", input.display(), output.display()));

        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let progress_thread = qt_thread.clone();
            let result = pdf2word_core::convert(&input, &output, &opts, Some(&token), &mut |p| {
                let _ = progress_thread.queue(move |mut obj| match p {
                    Progress::Opened { pages } => obj.as_mut().append_log(&format!("  opened: {pages} page(s)")),
                    Progress::PageStarted { page, done, total } => {
                        obj.as_mut().set_progress(done as f64 / total.max(1) as f64);
                        obj.as_mut().set_status(QString::from(&format!("Analysing page {page} ({}/{total})", done + 1)));
                    }
                    Progress::PageOcr { page } => {
                        obj.as_mut().set_status(QString::from(&format!("Running OCR on page {page}…")));
                        obj.as_mut().append_log(&format!("  page {page}: OCR"));
                    }
                    Progress::PageDone { done, total, .. } => obj.as_mut().set_progress(done as f64 / total.max(1) as f64),
                    Progress::Writing => obj.as_mut().set_status(QString::from("Writing DOCX…")),
                    Progress::Warning(w) => obj.as_mut().append_log(&format!("  ⚠ {w}")),
                    Progress::Finished { .. } => {}
                });
            });
            let secs = started.elapsed().as_secs_f32();
            let _ = qt_thread.queue(move |mut obj| {
                obj.as_mut().rust_mut().cancel = None;
                obj.as_mut().set_busy(false);
                match result {
                    Ok(r) => {
                        let msg = format!(
                            "Done in {secs:.1}s: {} page(s), {} paragraphs, {} tables, {} images{}{}",
                            r.pages,
                            r.paragraphs,
                            r.tables,
                            r.images,
                            if r.ocr_pages.is_empty() { String::new() } else { format!(", OCR on {} page(s)", r.ocr_pages.len()) },
                            if r.embedded_fonts.is_empty() { String::new() } else { format!(", {} embedded font(s)", r.embedded_fonts.len()) }
                        );
                        obj.as_mut().set_progress(1.0);
                        obj.as_mut().set_status(QString::from(&msg));
                        obj.as_mut().append_log(&format!("✔ {msg}"));
                        obj.as_mut().set_last_output(QString::from(&output.to_string_lossy().into_owned()));
                        obj.as_mut().finished(true, QString::from(&msg));
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        obj.as_mut().set_progress(0.0);
                        obj.as_mut().set_status(QString::from(&format!("Failed: {msg}")));
                        obj.as_mut().append_log(&format!("✖ {msg}"));
                        obj.as_mut().finished(false, QString::from(&msg));
                    }
                }
            });
        });
    }

    pub fn cancel(mut self: Pin<&mut Self>) {
        if let Some(t) = &self.as_mut().rust_mut().cancel {
            t.cancel();
        }
        self.as_mut().set_status(QString::from("Cancelling…"));
    }

    pub fn output_url(&self) -> QUrl {
        match opt_string(self.last_output()) {
            Some(p) => path_to_url(Path::new(&p)),
            None => QUrl::default(),
        }
    }

    pub fn output_folder_url(&self) -> QUrl {
        match opt_string(self.last_output()) {
            Some(p) => Path::new(&p).parent().map(path_to_url).unwrap_or_default(),
            None => QUrl::default(),
        }
    }
}
