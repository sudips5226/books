//! The `Converter` QObject exposed to QML.
//!
//! It keeps a queue of PDF files that are converted one after the other with
//! the same options. All PDF work (inspection, preview rendering, structure
//! analysis, conversion) runs on background threads; results are queued back
//! onto the Qt event loop with `CxxQtThread`. Options are saved between runs.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QList, QString, QUrl};
use pdf2word_core::{CancelToken, ConvertOptions, LayoutMode, OcrMode, Progress};
use serde_json::{json, Value};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qurl.h");
        type QUrl = cxx_qt_lib::QUrl;
        include!("cxx-qt-lib/qlist.h");
        type QList_QUrl = cxx_qt_lib::QList<QUrl>;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, input_path, cxx_name = "inputPath")]
        #[qproperty(QString, output_path, cxx_name = "outputPath")]
        #[qproperty(QString, output_folder, cxx_name = "outputFolder")]
        #[qproperty(i32, page_count, cxx_name = "pageCount")]
        #[qproperty(QString, document_info, cxx_name = "documentInfo")]
        #[qproperty(i32, preview_page, cxx_name = "previewPage")]
        #[qproperty(QUrl, preview_source, cxx_name = "previewSource")]
        #[qproperty(bool, show_structure, cxx_name = "showStructure")]
        #[qproperty(QString, structure_json, cxx_name = "structureJson")]
        #[qproperty(QString, queue_json, cxx_name = "queueJson")]
        #[qproperty(i32, selected)]
        #[qproperty(i32, pending)]
        #[qproperty(bool, busy)]
        #[qproperty(f64, progress)]
        #[qproperty(QString, status)]
        #[qproperty(QString, log)]
        #[qproperty(QString, last_output, cxx_name = "lastOutput")]
        #[qproperty(i32, layout_mode, cxx_name = "layoutMode")]
        #[qproperty(i32, ocr_mode, cxx_name = "ocrMode")]
        #[qproperty(QString, ocr_language, cxx_name = "ocrLanguage")]
        #[qproperty(i32, ocr_dpi, cxx_name = "ocrDpi")]
        #[qproperty(bool, straighten)]
        #[qproperty(bool, picture_text, cxx_name = "pictureText")]
        #[qproperty(QString, page_range, cxx_name = "pageRange")]
        #[qproperty(QString, password)]
        #[qproperty(bool, extract_images, cxx_name = "extractImages")]
        #[qproperty(bool, vector_figures, cxx_name = "vectorFigures")]
        #[qproperty(bool, vector_shapes, cxx_name = "vectorShapes")]
        #[qproperty(bool, detect_tables, cxx_name = "detectTables")]
        #[qproperty(bool, detect_headings, cxx_name = "detectHeadings")]
        #[qproperty(bool, headers_footers, cxx_name = "headersFooters")]
        #[qproperty(bool, word_lists, cxx_name = "wordLists")]
        #[qproperty(bool, paragraph_styles, cxx_name = "paragraphStyles")]
        #[qproperty(bool, navigation)]
        #[qproperty(bool, comments)]
        #[qproperty(bool, embed_fonts, cxx_name = "embedFonts")]
        #[qproperty(bool, keep_widths, cxx_name = "keepWidths")]
        #[qproperty(QString, ocr_engine, cxx_name = "ocrEngine")]
        #[qproperty(QString, screenshot_path, cxx_name = "screenshotPath")]
        #[qproperty(bool, auto_convert, cxx_name = "autoConvert")]
        type Converter = super::ConverterRust;
    }

    extern "RustQt" {
        /// Adds one PDF (file dialog, drop, command line) and selects it.
        #[qinvokable]
        #[cxx_name = "openPdf"]
        fn open_pdf(self: Pin<&mut Converter>, url: &QUrl);

        /// Adds several PDFs to the queue.
        #[qinvokable]
        #[cxx_name = "addUrls"]
        fn add_urls(self: Pin<&mut Converter>, urls: &QList_QUrl);

        /// Shows a queued file in the preview.
        #[qinvokable]
        #[cxx_name = "selectItem"]
        fn select_item(self: Pin<&mut Converter>, index: i32);

        #[qinvokable]
        #[cxx_name = "removeItem"]
        fn remove_item(self: Pin<&mut Converter>, index: i32);

        /// Removes every file that is not being converted.
        #[qinvokable]
        #[cxx_name = "clearQueue"]
        fn clear_queue(self: Pin<&mut Converter>);

        /// Sets the selected file's output from a save dialog URL.
        #[qinvokable]
        #[cxx_name = "setOutputUrl"]
        fn set_output_url(self: Pin<&mut Converter>, url: &QUrl);

        /// Sets the selected file's output path (typed in).
        #[qinvokable]
        #[cxx_name = "setOutputText"]
        fn set_output_text(self: Pin<&mut Converter>, text: &QString);

        /// Puts the documents of the waiting files into this folder (empty URL: next to each PDF).
        #[qinvokable]
        #[cxx_name = "setOutputFolderUrl"]
        fn set_output_folder_url(self: Pin<&mut Converter>, url: &QUrl);

        /// Renders the given (1-based) page into the preview.
        #[qinvokable]
        #[cxx_name = "showPage"]
        fn show_page(self: Pin<&mut Converter>, page: i32);

        /// Shows or hides the detected structure over the preview.
        #[qinvokable]
        #[cxx_name = "setStructureVisible"]
        fn set_structure_visible(self: Pin<&mut Converter>, on: bool);

        /// Converts every waiting (or failed) file in the queue.
        #[qinvokable]
        fn convert(self: Pin<&mut Converter>);

        #[qinvokable]
        fn cancel(self: Pin<&mut Converter>);

        /// URL of the selected file's document (for "Open").
        #[qinvokable]
        #[cxx_name = "outputUrl"]
        fn output_url(self: &Converter) -> QUrl;

        /// URL of the folder containing it.
        #[qinvokable]
        #[cxx_name = "outputFolderUrl"]
        fn output_folder_url(self: &Converter) -> QUrl;

        /// Opens the PDFs passed on the command line (file associations, "Open with…").
        #[qinvokable]
        #[cxx_name = "openFromArguments"]
        fn open_from_arguments(self: Pin<&mut Converter>);

        /// Saves the options for the next start.
        #[qinvokable]
        #[cxx_name = "saveSettings"]
        fn save_settings(self: &Converter);

        #[qsignal]
        fn finished(self: Pin<&mut Converter>, success: bool, message: QString);
    }

    impl cxx_qt::Threading for Converter {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemStatus {
    Waiting,
    Converting,
    Done,
    Failed,
}

#[derive(Clone, Debug)]
struct QueueItem {
    input: PathBuf,
    output: PathBuf,
    /// The output was chosen by hand (not derived from the output folder).
    custom_output: bool,
    status: ItemStatus,
    progress: f64,
    message: String,
}

pub struct ConverterRust {
    input_path: QString,
    output_path: QString,
    output_folder: QString,
    page_count: i32,
    document_info: QString,
    preview_page: i32,
    preview_source: QUrl,
    show_structure: bool,
    structure_json: QString,
    queue_json: QString,
    selected: i32,
    pending: i32,
    busy: bool,
    progress: f64,
    status: QString,
    log: QString,
    last_output: QString,
    layout_mode: i32,
    ocr_mode: i32,
    ocr_language: QString,
    ocr_dpi: i32,
    straighten: bool,
    picture_text: bool,
    page_range: QString,
    password: QString,
    extract_images: bool,
    vector_figures: bool,
    vector_shapes: bool,
    detect_tables: bool,
    detect_headings: bool,
    headers_footers: bool,
    word_lists: bool,
    paragraph_styles: bool,
    navigation: bool,
    comments: bool,
    embed_fonts: bool,
    keep_widths: bool,
    ocr_engine: QString,
    screenshot_path: QString,
    auto_convert: bool,

    queue: Vec<QueueItem>,
    cancel: Option<CancelToken>,
}

const EMPTY_INFO: &str = "Drop PDF files here or click “Open PDF…”";

/// Where the options are kept between runs.
fn settings_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("pdf2word").join("settings.json"))
}

impl Default for ConverterRust {
    fn default() -> Self {
        let mut s = Self {
            input_path: QString::default(),
            output_path: QString::default(),
            output_folder: QString::default(),
            page_count: 0,
            document_info: QString::from(EMPTY_INFO),
            preview_page: 1,
            preview_source: QUrl::default(),
            show_structure: false,
            structure_json: QString::from("[]"),
            queue_json: QString::from("[]"),
            selected: -1,
            pending: 0,
            busy: false,
            progress: 0.0,
            status: QString::from("Ready"),
            log: QString::default(),
            last_output: QString::default(),
            layout_mode: 0,
            ocr_mode: 0,
            ocr_language: QString::from("auto"),
            ocr_dpi: 300,
            straighten: true,
            picture_text: true,
            page_range: QString::default(),
            password: QString::default(),
            extract_images: true,
            vector_figures: true,
            vector_shapes: true,
            detect_tables: true,
            detect_headings: true,
            headers_footers: true,
            word_lists: true,
            paragraph_styles: true,
            navigation: true,
            comments: true,
            embed_fonts: true,
            keep_widths: true,
            ocr_engine: QString::from(&format!("Tesseract {}", pdf2word_core::ocr_version())),
            screenshot_path: QString::from(&std::env::var("PDF2WORD_SCREENSHOT").unwrap_or_default()),
            auto_convert: std::env::var_os("PDF2WORD_AUTOCONVERT").is_some(),
            queue: Vec::new(),
            cancel: None,
        };
        if let Some(v) = settings_file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            s.apply_settings(&v);
        }
        s
    }
}

impl ConverterRust {
    fn settings_json(&self) -> Value {
        json!({
            "layoutMode": self.layout_mode,
            "ocrMode": self.ocr_mode,
            "ocrLanguage": self.ocr_language.to_string(),
            "ocrDpi": self.ocr_dpi,
            "straighten": self.straighten,
            "pictureText": self.picture_text,
            "extractImages": self.extract_images,
            "vectorFigures": self.vector_figures,
            "vectorShapes": self.vector_shapes,
            "detectTables": self.detect_tables,
            "detectHeadings": self.detect_headings,
            "headersFooters": self.headers_footers,
            "wordLists": self.word_lists,
            "paragraphStyles": self.paragraph_styles,
            "navigation": self.navigation,
            "comments": self.comments,
            "embedFonts": self.embed_fonts,
            "keepWidths": self.keep_widths,
            "outputFolder": self.output_folder.to_string(),
            "showStructure": self.show_structure,
        })
    }

    fn apply_settings(&mut self, v: &Value) {
        let b = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
        let i = |k: &str, d: i32| v.get(k).and_then(Value::as_i64).map(|x| x as i32).unwrap_or(d);
        let st = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        self.layout_mode = i("layoutMode", self.layout_mode).clamp(0, 2);
        self.ocr_mode = i("ocrMode", self.ocr_mode).clamp(0, 2);
        if let Some(l) = st("ocrLanguage").filter(|l| !l.trim().is_empty()) {
            self.ocr_language = QString::from(&l);
        }
        self.ocr_dpi = i("ocrDpi", self.ocr_dpi).clamp(100, 600);
        self.straighten = b("straighten", self.straighten);
        self.picture_text = b("pictureText", self.picture_text);
        self.extract_images = b("extractImages", self.extract_images);
        self.vector_figures = b("vectorFigures", self.vector_figures);
        self.vector_shapes = b("vectorShapes", self.vector_shapes);
        self.detect_tables = b("detectTables", self.detect_tables);
        self.detect_headings = b("detectHeadings", self.detect_headings);
        self.headers_footers = b("headersFooters", self.headers_footers);
        self.word_lists = b("wordLists", self.word_lists);
        self.paragraph_styles = b("paragraphStyles", self.paragraph_styles);
        self.navigation = b("navigation", self.navigation);
        self.comments = b("comments", self.comments);
        self.embed_fonts = b("embedFonts", self.embed_fonts);
        self.keep_widths = b("keepWidths", self.keep_widths);
        self.show_structure = b("showStructure", self.show_structure);
        if let Some(f) = st("outputFolder").filter(|f| Path::new(f).is_dir()) {
            self.output_folder = QString::from(&f);
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

fn qs(p: &Path) -> QString {
    QString::from(&p.to_string_lossy().into_owned())
}

fn output_for(input: &Path, folder: Option<&Path>) -> PathBuf {
    match (folder, input.file_stem()) {
        (Some(f), Some(stem)) => f.join(stem).with_extension("docx"),
        _ => input.with_extension("docx"),
    }
}

/// The page's structure as JSON for the overlay.
fn structure_json(input: &Path, password: Option<&str>, page: i32) -> Option<String> {
    let boxes = pdf2word_core::page_structure(input, password, (page - 1).max(0) as usize, None).ok()?;
    let v: Vec<Value> = boxes.iter().map(|b| json!({ "kind": b.kind, "x": b.x, "y": b.y, "w": b.w, "h": b.h, "label": b.label })).collect();
    Some(Value::Array(v).to_string())
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

    fn folder(&self) -> Option<PathBuf> {
        opt_string(self.output_folder()).map(PathBuf::from).filter(|p| p.is_dir())
    }

    /// Publishes the queue to QML.
    fn sync_queue(mut self: Pin<&mut Self>) {
        let items: Vec<Value> = self
            .rust()
            .queue
            .iter()
            .map(|q| {
                json!({
                    "name": q.input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    "path": q.input.to_string_lossy(),
                    "output": q.output.to_string_lossy(),
                    "status": match q.status {
                        ItemStatus::Waiting => "waiting",
                        ItemStatus::Converting => "converting",
                        ItemStatus::Done => "done",
                        ItemStatus::Failed => "failed",
                    },
                    "progress": q.progress,
                    "message": q.message,
                })
            })
            .collect();
        let pending = self.rust().queue.iter().filter(|q| matches!(q.status, ItemStatus::Waiting | ItemStatus::Failed)).count() as i32;
        self.as_mut().set_queue_json(QString::from(&Value::Array(items).to_string()));
        self.as_mut().set_pending(pending);
        let sel = *self.selected();
        if let Some(q) = usize::try_from(sel).ok().and_then(|i| self.rust().queue.get(i).cloned()) {
            self.as_mut().set_output_path(qs(&q.output));
            self.as_mut().set_last_output(if q.status == ItemStatus::Done { qs(&q.output) } else { QString::default() });
        }
    }

    /// Nothing selected (empty queue).
    fn reset_selection(mut self: Pin<&mut Self>) {
        self.as_mut().set_selected(-1);
        self.as_mut().set_input_path(QString::default());
        self.as_mut().set_output_path(QString::default());
        self.as_mut().set_last_output(QString::default());
        self.as_mut().set_page_count(0);
        self.as_mut().set_preview_source(QUrl::default());
        self.as_mut().set_structure_json(QString::from("[]"));
        self.as_mut().set_document_info(QString::from(EMPTY_INFO));
        self.sync_queue();
    }

    fn add_path(mut self: Pin<&mut Self>, path: PathBuf) -> usize {
        if let Some(i) = self.rust().queue.iter().position(|q| q.input == path) {
            return i;
        }
        let output = output_for(&path, self.folder().as_deref());
        self.as_mut().rust_mut().queue.push(QueueItem { input: path, output, custom_output: false, status: ItemStatus::Waiting, progress: 0.0, message: String::new() });
        self.rust().queue.len() - 1
    }

    pub fn open_pdf(mut self: Pin<&mut Self>, url: &QUrl) {
        let Some(path) = url_to_path(url) else {
            self.as_mut().set_status(QString::from("Only local files can be opened"));
            return;
        };
        let i = self.as_mut().add_path(path);
        self.as_mut().sync_queue();
        self.select_item(i as i32);
    }

    pub fn add_urls(mut self: Pin<&mut Self>, urls: &QList<QUrl>) {
        let mut first = None;
        let mut count = 0;
        for url in urls.iter() {
            if let Some(p) = url_to_path(url).filter(|p| p.is_file()) {
                let i = self.as_mut().add_path(p);
                first.get_or_insert(i);
                count += 1;
            }
        }
        self.as_mut().sync_queue();
        if let Some(i) = first {
            if *self.selected() < 0 || count == 1 {
                self.select_item(i as i32);
            }
        }
    }

    pub fn select_item(mut self: Pin<&mut Self>, index: i32) {
        let Some(item) = usize::try_from(index).ok().and_then(|i| self.rust().queue.get(i).cloned()) else {
            return;
        };
        let same = *self.selected() == index && self.input_path().to_string() == item.input.to_string_lossy();
        self.as_mut().set_selected(index);
        self.as_mut().sync_queue();
        if same && *self.page_count() > 0 {
            return;
        }
        self.as_mut().set_input_path(qs(&item.input));
        self.as_mut().set_page_count(0);
        self.as_mut().set_structure_json(QString::from("[]"));
        self.as_mut().set_document_info(QString::from("Reading document…"));

        let path = item.input.clone();
        let password = opt_string(self.password());
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = pdf2word_core::inspect(&path, password.as_deref(), None);
            let _ = qt_thread.queue(move |mut obj| {
                // Ignore answers for a file that is no longer selected.
                if obj.input_path().to_string() != path.to_string_lossy() {
                    return;
                }
                match result {
                    Ok(info) => {
                        let mut text = format!("{} page{}", info.pages, if info.pages == 1 { "" } else { "s" });
                        if let Some(t) = info.title.filter(|t| !t.is_empty()) {
                            text = format!("{t} — {text}");
                        }
                        if info.scanned_pages_sampled > 0 {
                            text.push_str(&format!(" · scanned pages detected ({} of first {}) — OCR will be used", info.scanned_pages_sampled, info.sampled));
                        }
                        obj.as_mut().set_page_count(info.pages as i32);
                        obj.as_mut().set_document_info(QString::from(&text));
                        obj.as_mut().show_page(1);
                    }
                    Err(e) => {
                        obj.as_mut().set_document_info(QString::from(&format!("Cannot open: {e:#}")));
                        obj.as_mut().set_preview_source(QUrl::default());
                    }
                }
            });
        });
    }

    pub fn remove_item(mut self: Pin<&mut Self>, index: i32) {
        let Ok(i) = usize::try_from(index) else { return };
        if self.rust().queue.get(i).is_none_or(|q| q.status == ItemStatus::Converting) {
            return;
        }
        self.as_mut().rust_mut().queue.remove(i);
        let len = self.rust().queue.len() as i32;
        let sel = *self.selected();
        if len == 0 {
            self.reset_selection();
        } else if sel >= index {
            let next = if sel == index { sel.min(len - 1) } else { sel - 1 };
            self.as_mut().set_selected(-1);
            self.as_mut().sync_queue();
            self.select_item(next);
        } else {
            self.sync_queue();
        }
    }

    pub fn clear_queue(mut self: Pin<&mut Self>) {
        let keep: Vec<QueueItem> = self.rust().queue.iter().filter(|q| q.status == ItemStatus::Converting).cloned().collect();
        let empty = keep.is_empty();
        self.as_mut().rust_mut().queue = keep;
        if empty {
            self.reset_selection();
        } else {
            self.as_mut().set_selected(-1);
            self.as_mut().sync_queue();
            self.select_item(0);
        }
    }

    pub fn open_from_arguments(mut self: Pin<&mut Self>) {
        let paths: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .filter(|a| !a.to_string_lossy().starts_with('-'))
            .map(|a| std::fs::canonicalize(&a).unwrap_or_else(|_| PathBuf::from(&a)))
            .filter(|p| p.is_file())
            .collect();
        let mut first = None;
        for p in paths {
            let i = self.as_mut().add_path(p);
            first.get_or_insert(i);
        }
        self.as_mut().sync_queue();
        if let Some(i) = first {
            self.select_item(i as i32);
        }
    }

    fn set_selected_output(mut self: Pin<&mut Self>, mut p: PathBuf) {
        if p.extension().is_none_or(|e| !e.eq_ignore_ascii_case("docx")) {
            p.set_extension("docx");
        }
        if let Ok(i) = usize::try_from(*self.selected()) {
            let mut rust = self.as_mut().rust_mut();
            if let Some(item) = rust.queue.get_mut(i) {
                item.output = p.clone();
                item.custom_output = true;
            }
        }
        self.as_mut().set_output_path(qs(&p));
        self.sync_queue();
    }

    pub fn set_output_url(self: Pin<&mut Self>, url: &QUrl) {
        if let Some(p) = url_to_path(url) {
            self.set_selected_output(p);
        }
    }

    pub fn set_output_text(self: Pin<&mut Self>, text: &QString) {
        if let Some(t) = opt_string(text) {
            self.set_selected_output(PathBuf::from(t));
        }
    }

    pub fn set_output_folder_url(mut self: Pin<&mut Self>, url: &QUrl) {
        let folder = url_to_path(url).filter(|p| p.is_dir());
        self.as_mut().set_output_folder(folder.as_deref().map(qs).unwrap_or_default());
        for item in self.as_mut().rust_mut().queue.iter_mut() {
            if !item.custom_output && item.status != ItemStatus::Converting {
                item.output = output_for(&item.input, folder.as_deref());
            }
        }
        self.sync_queue();
    }

    pub fn show_page(mut self: Pin<&mut Self>, page: i32) {
        let count = *self.page_count();
        if count <= 0 {
            return;
        }
        let page = page.clamp(1, count);
        self.as_mut().set_preview_page(page);
        self.as_mut().set_structure_json(QString::from("[]"));
        let input = PathBuf::from(self.input_path().to_string());
        let password = opt_string(self.password());
        let structure = *self.show_structure();
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let n = PREVIEW_COUNTER.fetch_add(1, Ordering::SeqCst);
            let out = std::env::temp_dir().join(format!("pdf2word-preview-{}-{n}.png", std::process::id()));
            let result = pdf2word_core::render_page_png(&input, password.as_deref(), (page - 1) as usize, 900, &out, None);
            let boxes = if structure { structure_json(&input, password.as_deref(), page) } else { None };
            let _ = qt_thread.queue(move |mut obj| {
                // Ignore stale renders.
                if *obj.preview_page() != page || obj.input_path().to_string() != input.to_string_lossy() {
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
                if let Some(b) = boxes.filter(|_| *obj.show_structure()) {
                    obj.as_mut().set_structure_json(QString::from(&b));
                }
            });
        });
    }

    pub fn set_structure_visible(mut self: Pin<&mut Self>, on: bool) {
        self.as_mut().set_show_structure(on);
        if !on {
            self.as_mut().set_structure_json(QString::from("[]"));
            return;
        }
        let page = *self.preview_page();
        let input = PathBuf::from(self.input_path().to_string());
        if !input.is_file() {
            return;
        }
        let password = opt_string(self.password());
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let boxes = structure_json(&input, password.as_deref(), page);
            let _ = qt_thread.queue(move |mut obj| {
                if *obj.preview_page() == page && *obj.show_structure() && obj.input_path().to_string() == input.to_string_lossy() {
                    if let Some(b) = boxes {
                        obj.as_mut().set_structure_json(QString::from(&b));
                    }
                }
            });
        });
    }

    fn options(&self) -> ConvertOptions {
        let mut o = ConvertOptions {
            layout_mode: match *self.layout_mode() {
                1 => LayoutMode::Continuous,
                2 => LayoutMode::Exact,
                _ => LayoutMode::Flow,
            },
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
            headers_footers: *self.headers_footers(),
            word_lists: *self.word_lists(),
            footnotes: *self.word_lists(),
            paragraph_styles: *self.paragraph_styles(),
            navigation: *self.navigation(),
            annotations: *self.comments(),
            straighten_scans: *self.straighten(),
            picture_text: *self.picture_text(),
            ..Default::default()
        };
        o.ocr.language = opt_string(self.ocr_language()).unwrap_or_else(|| "auto".into());
        o.extract.extract_images = *self.extract_images();
        o.extract.rasterize_vector_graphics = *self.vector_figures() && *self.extract_images();
        o.extract.vector_shapes = *self.vector_shapes();
        o.layout.detect_tables = *self.detect_tables();
        o
    }

    pub fn convert(mut self: Pin<&mut Self>) {
        if *self.busy() {
            return;
        }
        let jobs: Vec<(usize, PathBuf, PathBuf)> = self
            .rust()
            .queue
            .iter()
            .enumerate()
            .filter(|(_, q)| matches!(q.status, ItemStatus::Waiting | ItemStatus::Failed))
            .map(|(i, q)| (i, q.input.clone(), q.output.clone()))
            .collect();
        if jobs.is_empty() {
            self.as_mut().set_status(QString::from("Add a PDF file first"));
            return;
        }
        self.save_settings();
        let opts = self.options();
        // A page range only makes sense for a single file.
        let single = jobs.len() == 1;
        let token = CancelToken::new();
        self.as_mut().rust_mut().cancel = Some(token.clone());
        self.as_mut().set_busy(true);
        self.as_mut().set_progress(0.0);
        self.as_mut().set_status(QString::from("Converting…"));

        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let total = jobs.len();
            let (mut ok, mut failed) = (0usize, 0usize);
            let started = std::time::Instant::now();
            for (k, (index, input, output)) in jobs.into_iter().enumerate() {
                if token.is_cancelled() {
                    break;
                }
                let mut opts = opts.clone();
                if !single {
                    opts.pages = None;
                }
                let line = format!("▶ {} → {}", input.display(), output.display());
                let _ = qt_thread.queue(move |mut obj| {
                    obj.as_mut().append_log(&line);
                    if let Some(item) = obj.as_mut().rust_mut().queue.get_mut(index) {
                        item.status = ItemStatus::Converting;
                        item.progress = 0.0;
                        item.message.clear();
                    }
                    obj.as_mut().sync_queue();
                });
                if let Some(dir) = output.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let progress_thread = qt_thread.clone();
                let file_start = std::time::Instant::now();
                let result = pdf2word_core::convert(&input, &output, &opts, Some(&token), &mut |p| {
                    let _ = progress_thread.queue(move |mut obj| {
                        let set_item = |obj: &mut Pin<&mut qobject::Converter>, f: f64| {
                            if let Some(item) = obj.as_mut().rust_mut().queue.get_mut(index) {
                                item.progress = f;
                            }
                            obj.as_mut().set_progress((k as f64 + f) / total as f64);
                        };
                        match p {
                            Progress::Opened { pages } => obj.as_mut().append_log(&format!("  opened: {pages} page(s)")),
                            Progress::PageStarted { page, done, total: pages } => {
                                set_item(&mut obj, done as f64 / pages.max(1) as f64 * 0.5);
                                let prefix = if total > 1 { format!("File {}/{total}: ", k + 1) } else { String::new() };
                                obj.as_mut().set_status(QString::from(&format!("{prefix}analysing page {page} ({}/{pages})", done + 1)));
                            }
                            Progress::PageOcr { page } => obj.as_mut().append_log(&format!("  page {page}: OCR")),
                            Progress::PageDone { done, total: pages, .. } => {
                                set_item(&mut obj, done as f64 / pages.max(1) as f64 * 0.95);
                                obj.as_mut().sync_queue();
                            }
                            Progress::Writing => obj.as_mut().set_status(QString::from("Writing DOCX…")),
                            Progress::Warning(w) => obj.as_mut().append_log(&format!("  ⚠ {w}")),
                            Progress::Finished { .. } => {}
                        }
                    });
                });
                let secs = file_start.elapsed().as_secs_f32();
                let (status, message) = match &result {
                    Ok(r) => {
                        ok += 1;
                        let ocr = if r.ocr_pages.is_empty() { String::new() } else { format!(", OCR on {} page(s)", r.ocr_pages.len()) };
                        (ItemStatus::Done, format!("{} page(s), {} paragraphs, {} tables, {} images{ocr} in {secs:.1}s", r.pages, r.paragraphs, r.tables, r.images))
                    }
                    Err(e) => {
                        failed += 1;
                        (ItemStatus::Failed, format!("{e:#}"))
                    }
                };
                let _ = qt_thread.queue(move |mut obj| {
                    let mark = if status == ItemStatus::Done { "✔" } else { "✖" };
                    obj.as_mut().append_log(&format!("{mark} {message}"));
                    if let Some(item) = obj.as_mut().rust_mut().queue.get_mut(index) {
                        item.status = status;
                        item.progress = if status == ItemStatus::Done { 1.0 } else { 0.0 };
                        item.message = message;
                    }
                    obj.as_mut().sync_queue();
                });
            }
            let cancelled = token.is_cancelled();
            let secs = started.elapsed().as_secs_f32();
            let _ = qt_thread.queue(move |mut obj| {
                obj.as_mut().rust_mut().cancel = None;
                // Files interrupted by a cancel go back to waiting.
                for item in obj.as_mut().rust_mut().queue.iter_mut() {
                    if item.status == ItemStatus::Converting {
                        item.status = ItemStatus::Waiting;
                        item.progress = 0.0;
                    }
                }
                obj.as_mut().set_busy(false);
                obj.as_mut().sync_queue();
                let msg = if cancelled {
                    "Cancelled".to_string()
                } else if failed == 0 {
                    format!("{ok} Word document{} created in {secs:.1}s", if ok == 1 { "" } else { "s" })
                } else {
                    format!("{ok} converted, {failed} failed")
                };
                let success = failed == 0 && !cancelled;
                obj.as_mut().set_progress(if success { 1.0 } else { 0.0 });
                obj.as_mut().set_status(QString::from(&msg));
                obj.as_mut().finished(success, QString::from(&msg));
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

    pub fn save_settings(&self) {
        let Some(path) = settings_file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = serde_json::to_string_pretty(&self.rust().settings_json()).unwrap_or_default();
        if let Err(e) = std::fs::write(&path, text) {
            log::warn!("could not save settings to {}: {e}", path.display());
        }
    }
}
