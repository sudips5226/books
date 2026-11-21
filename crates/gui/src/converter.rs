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
        #[qproperty(bool, edit_mode, cxx_name = "editMode")]
        #[qproperty(QString, page_text_json, cxx_name = "pageTextJson")]
        #[qproperty(i32, edit_count, cxx_name = "editCount")]
        #[qproperty(i32, redo_count, cxx_name = "redoCount")]
        #[qproperty(bool, edit_busy, cxx_name = "editBusy")]
        #[qproperty(bool, previewing)]
        #[qproperty(bool, ocr_busy, cxx_name = "ocrBusy")]
        #[qproperty(QString, find_json, cxx_name = "findJson")]
        #[qproperty(bool, find_busy, cxx_name = "findBusy")]
        #[qproperty(QString, edit_demo, cxx_name = "editDemo")]
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

        /// Shows the page's text runs for editing.
        #[qinvokable]
        #[cxx_name = "enableEditing"]
        fn set_edit_mode_on(self: Pin<&mut Converter>, on: bool);

        /// Changes run `index` (from `pageTextJson`): its text, and its look
        /// from `style` (JSON: bold, italic, size, color "#rrggbb").
        #[qinvokable]
        #[cxx_name = "editRun"]
        fn edit_run(self: Pin<&mut Converter>, index: i32, text: &QString, fit: bool, style: &QString);

        /// Shows what `editRun` would do, without keeping it.
        #[qinvokable]
        #[cxx_name = "previewRun"]
        fn preview_run(self: Pin<&mut Converter>, index: i32, text: &QString, fit: bool, style: &QString);

        /// Rewrites paragraph `index`, wrapping it over as many lines as it needs.
        #[qinvokable]
        #[cxx_name = "editParagraph"]
        fn edit_paragraph(self: Pin<&mut Converter>, index: i32, text: &QString, fit: bool, push_down: bool);

        #[qinvokable]
        #[cxx_name = "previewParagraph"]
        fn preview_paragraph(self: Pin<&mut Converter>, index: i32, text: &QString, fit: bool, push_down: bool);

        /// Back to the page as it is.
        #[qinvokable]
        #[cxx_name = "cancelPreview"]
        fn cancel_preview(self: Pin<&mut Converter>);

        /// Deletes runs (JSON array of indices).
        #[qinvokable]
        #[cxx_name = "deleteItems"]
        fn delete_items(self: Pin<&mut Converter>, indices: &QString);

        /// Moves run `index` by (dx, dy), fractions of the page.
        #[qinvokable]
        #[cxx_name = "moveText"]
        fn move_text(self: Pin<&mut Converter>, index: i32, dx: f64, dy: f64);

        /// Adds text at (x, y), fractions of the page. `style`: family (0 sans,
        /// 1 serif, 2 monospace) + 4 for bold + 8 for italic.
        #[qinvokable]
        #[cxx_name = "insertText"]
        fn insert_text(self: Pin<&mut Converter>, x: f64, y: f64, text: &QString, size: f64, style: i32);

        /// Searches the document (results in `findJson`).
        #[qinvokable]
        #[cxx_name = "findText"]
        fn find_text(self: Pin<&mut Converter>, query: &QString, match_case: bool, whole_word: bool);

        /// Replaces the found matches with these indices (JSON array).
        #[qinvokable]
        #[cxx_name = "replaceFound"]
        fn replace_found(self: Pin<&mut Converter>, indices: &QString, replacement: &QString);

        /// Recognises letters drawn as outlines on the page (OCR) so they can be edited.
        #[qinvokable]
        #[cxx_name = "recognizeOutlined"]
        fn recognize_outlined(self: Pin<&mut Converter>);

        /// Takes back the last edit of the selected file.
        #[qinvokable]
        #[cxx_name = "undoEdit"]
        fn undo_edit(self: Pin<&mut Converter>);

        #[qinvokable]
        #[cxx_name = "redoEdit"]
        fn redo_edit(self: Pin<&mut Converter>);

        /// Changes the current page: "rotate-left", "rotate-right", "delete",
        /// "move-earlier", "move-later", "insert-blank".
        #[qinvokable]
        #[cxx_name = "pageOp"]
        fn page_op(self: Pin<&mut Converter>, kind: &QString);

        /// Inserts the pages of another PDF after the current page.
        #[qinvokable]
        #[cxx_name = "insertPdf"]
        fn insert_pdf(self: Pin<&mut Converter>, url: &QUrl);

        /// Writes pages (e.g. "1-3,7") of the selected file to a new PDF.
        #[qinvokable]
        #[cxx_name = "extractPages"]
        fn extract_pages(self: Pin<&mut Converter>, spec: &QString, url: &QUrl);

        /// Splits the selected file into PDFs of `every` pages in a folder.
        #[qinvokable]
        #[cxx_name = "splitPdf"]
        fn split_pdf(self: Pin<&mut Converter>, every: i32, folder: &QUrl);

        /// Joins all listed files (with their edits) into one PDF and adds it to the list.
        #[qinvokable]
        #[cxx_name = "mergeFiles"]
        fn merge_files(self: Pin<&mut Converter>, url: &QUrl);

        /// Permanently removes everything in a box (fractions of the page) and blacks it out.
        #[qinvokable]
        #[cxx_name = "redactArea"]
        fn redact_area(self: Pin<&mut Converter>, x: f64, y: f64, w: f64, h: f64);

        /// Redacts the found matches with these indices (JSON array).
        #[qinvokable]
        #[cxx_name = "redactFound"]
        fn redact_found(self: Pin<&mut Converter>, indices: &QString);

        /// Deletes picture `index` (from `pageTextJson`).
        #[qinvokable]
        #[cxx_name = "deleteImage"]
        fn delete_image(self: Pin<&mut Converter>, index: i32);

        /// Moves/resizes picture `index` into a box (fractions of the page).
        #[qinvokable]
        #[cxx_name = "placeImage"]
        fn place_image(self: Pin<&mut Converter>, index: i32, x: f64, y: f64, w: f64, h: f64);

        /// Shows another picture file in place of picture `index`.
        #[qinvokable]
        #[cxx_name = "replaceImage"]
        fn replace_image(self: Pin<&mut Converter>, index: i32, url: &QUrl);

        /// Adds a picture file in the middle of the page.
        #[qinvokable]
        #[cxx_name = "insertImage"]
        fn insert_image(self: Pin<&mut Converter>, url: &QUrl);

        /// Number of files with edits that were not saved as PDF.
        #[qinvokable]
        #[cxx_name = "unsavedFiles"]
        fn unsaved_files(self: &Converter) -> i32;

        /// Saves every edited file as `<name>-edited.pdf` in a folder.
        #[qinvokable]
        #[cxx_name = "saveAllEdited"]
        fn save_all_edited(self: Pin<&mut Converter>, folder: &QUrl);

        /// Forgets the edits of every file.
        #[qinvokable]
        #[cxx_name = "discardAllEdits"]
        fn discard_all_edits(self: Pin<&mut Converter>);

        /// Saves the selected file's edited PDF.
        #[qinvokable]
        #[cxx_name = "saveEditedPdf"]
        fn save_edited_pdf(self: Pin<&mut Converter>, url: &QUrl);

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
    /// Edited copies of the PDF, oldest first (the last one is current).
    versions: Vec<PathBuf>,
    /// Undone versions, most recently undone last.
    redo: Vec<PathBuf>,
    /// The current version was saved as a PDF.
    saved: Option<PathBuf>,
}

impl QueueItem {
    /// The PDF as it is now (with the user's text edits).
    fn source(&self) -> PathBuf {
        self.versions.last().cloned().unwrap_or_else(|| self.input.clone())
    }

    fn discard_versions(&mut self) {
        for v in self.versions.drain(..).chain(self.redo.drain(..)) {
            let _ = std::fs::remove_file(v);
        }
        if let Some(dir) = session_dir(&self.input) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Records the versions so that they come back when the file is opened again.
    fn save_session(&self) {
        let Some(dir) = session_dir(&self.input) else { return };
        if self.versions.is_empty() && self.redo.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let (size, mtime) = file_stamp(&self.input).unwrap_or((0, 0));
        let names = |v: &[PathBuf]| v.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let v = json!({ "input": self.input.to_string_lossy(), "size": size, "mtime": mtime, "versions": names(&self.versions), "redo": names(&self.redo) });
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("session.json"), v.to_string());
    }

    fn clear_redo(&mut self) {
        for v in self.redo.drain(..) {
            let _ = std::fs::remove_file(v);
        }
    }
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
    edit_mode: bool,
    page_text_json: QString,
    edit_count: i32,
    redo_count: i32,
    edit_busy: bool,
    previewing: bool,
    ocr_busy: bool,
    find_json: QString,
    find_busy: bool,
    edit_demo: QString,

    queue: Vec<QueueItem>,
    cancel: Option<CancelToken>,
    /// Text of the previewed page (`page_text_json`): runs, then runs recognised in outlines.
    page_text: Option<pdf2word_core::edit::PageText>,
    outlined: Vec<pdf2word_core::edit::TextItem>,
    /// Pages whose outlines are recognised (file, page).
    ocr_pages: std::collections::HashSet<(PathBuf, i32)>,
    /// Search results and the version they were found in.
    matches: Vec<pdf2word_core::edit::Match>,
    matches_source: PathBuf,
    /// An edit shown as a preview: its request, result file and the version it applies to.
    preview_edit: Option<(String, PathBuf, PathBuf)>,
    /// A preview asked for while another edit was running.
    queued_preview: Option<(String, String, EditJob)>,
    /// Page to show after the running edit (1-based), e.g. after moving a page.
    goto_after_edit: Option<i32>,
    /// Incremented for every preview render, so that stale results are dropped.
    render_gen: u64,
}

impl Drop for ConverterRust {
    fn drop(&mut self) {
        // Edited versions stay in their sessions (restored when the file is
        // opened again); only an unused preview is removed.
        if let Some((_, path, _)) = self.preview_edit.take() {
            let _ = std::fs::remove_file(path);
        }
    }
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

/// Where the edited versions of a PDF are kept between runs.
fn session_dir(input: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("APPDATA")).map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    }?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    input.to_string_lossy().hash(&mut h);
    Some(base.join("pdf2word").join("sessions").join(format!("{:016x}", h.finish())))
}

/// Size and modification time of a file (to tell whether it changed).
fn file_stamp(p: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(p).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((m.len(), t))
}

/// The versions kept for `input`, when the file has not changed since.
fn load_session(input: &Path) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    let dir = session_dir(input)?;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("session.json")).ok()?).ok()?;
    let (size, mtime) = file_stamp(input)?;
    if v.get("size").and_then(Value::as_u64) != Some(size) || v.get("mtime").and_then(Value::as_u64) != Some(mtime) {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let paths = |k: &str| -> Vec<PathBuf> {
        v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(PathBuf::from).filter(|p| p.is_file()).collect()).unwrap_or_default()
    };
    let (versions, redo) = (paths("versions"), paths("redo"));
    (!versions.is_empty() || !redo.is_empty()).then_some((versions, redo))
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
            edit_mode: false,
            page_text_json: QString::from("{}"),
            edit_count: 0,
            redo_count: 0,
            edit_busy: false,
            previewing: false,
            ocr_busy: false,
            find_json: QString::from("[]"),
            find_busy: false,
            edit_demo: QString::from(&std::env::var("PDF2WORD_EDIT_DEMO").unwrap_or_default()),
            queue: Vec::new(),
            cancel: None,
            page_text: None,
            outlined: Vec::new(),
            ocr_pages: Default::default(),
            matches: Vec::new(),
            matches_source: PathBuf::new(),
            preview_edit: None,
            queued_preview: None,
            goto_after_edit: None,
            render_gen: 0,
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

/// An edit of a PDF: (source, password, output) → (changes made, notes for the user).
type EditJob = Box<dyn FnOnce(&Path, Option<&str>, &Path) -> anyhow::Result<(usize, Vec<String>)> + Send>;

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

/// "#rrggbb" → RGB.
fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
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
                    "edits": q.versions.len(),
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
            self.as_mut().set_edit_count(q.versions.len() as i32);
            self.as_mut().set_redo_count(q.redo.len() as i32);
        } else {
            self.as_mut().set_edit_count(0);
            self.as_mut().set_redo_count(0);
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
        self.as_mut().clear_page_text();
        self.as_mut().set_document_info(QString::from(EMPTY_INFO));
        self.sync_queue();
    }

    fn add_path(mut self: Pin<&mut Self>, path: PathBuf) -> usize {
        if let Some(i) = self.rust().queue.iter().position(|q| q.input == path) {
            return i;
        }
        let output = output_for(&path, self.folder().as_deref());
        let (versions, redo) = load_session(&path).unwrap_or_default();
        if !versions.is_empty() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.as_mut().set_status(QString::from(&format!("{name}: {} edit{} from last time restored", versions.len(), if versions.len() == 1 { "" } else { "s" })));
        }
        self.as_mut().rust_mut().queue.push(QueueItem {
            input: path,
            output,
            custom_output: false,
            status: ItemStatus::Waiting,
            progress: 0.0,
            message: String::new(),
            versions,
            redo,
            saved: None,
        });
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
        self.as_mut().clear_page_text();
        self.as_mut().set_document_info(QString::from("Reading document…"));

        let path = item.input.clone();
        let source = item.source();
        let password = opt_string(self.password());
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = pdf2word_core::inspect(&source, password.as_deref(), None);
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
        self.as_mut().rust_mut().queue.remove(i).discard_versions();
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
        let mut all = std::mem::take(&mut self.as_mut().rust_mut().queue);
        let (keep, mut gone): (Vec<QueueItem>, Vec<QueueItem>) = all.drain(..).partition(|q| q.status == ItemStatus::Converting);
        for q in &mut gone {
            q.discard_versions();
        }
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
        self.as_mut().clear_page_text();
        self.as_mut().rust_mut().render_gen += 1;
        let gen = self.rust().render_gen;
        let input = PathBuf::from(self.input_path().to_string());
        let source = self.selected_source().unwrap_or_else(|| input.clone());
        let password = opt_string(self.password());
        let structure = *self.show_structure();
        let editing = *self.edit_mode();
        let ocr = self.rust().ocr_pages.contains(&(input.clone(), page));
        let ocr_language = opt_string(self.ocr_language()).unwrap_or_else(|| "auto".into());
        self.as_mut().drop_pending();
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let n = PREVIEW_COUNTER.fetch_add(1, Ordering::SeqCst);
            let out = std::env::temp_dir().join(format!("pdf2word-preview-{}-{n}.png", std::process::id()));
            let result = pdf2word_core::render_page_png(&source, password.as_deref(), (page - 1) as usize, 1200, &out, None);
            let boxes = if structure { structure_json(&source, password.as_deref(), page) } else { None };
            let items = if editing { pdf2word_core::edit::page_text(&source, password.as_deref(), (page - 1) as usize, None).ok() } else { None };
            let outlined = if editing && ocr { pdf2word_core::edit::outlined_text(&source, password.as_deref(), (page - 1) as usize, &ocr_language, None).ok() } else { None };
            let _ = qt_thread.queue(move |mut obj| {
                // Ignore stale renders.
                if obj.rust().render_gen != gen || *obj.preview_page() != page || obj.input_path().to_string() != input.to_string_lossy() {
                    let _ = std::fs::remove_file(&out);
                    return;
                }
                if let Some(pt) = items.filter(|_| *obj.edit_mode()) {
                    obj.as_mut().rust_mut().outlined = outlined.unwrap_or_default();
                    obj.as_mut().set_page_text_value(pt);
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
        let source = self.selected_source().unwrap_or_else(|| input.clone());
        let password = opt_string(self.password());
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let boxes = structure_json(&source, password.as_deref(), page);
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
            .map(|(i, q)| (i, q.source(), q.output.clone()))
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
                let line = format!("▶ {} → {}", input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), output.display());
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

    /// The selected file's current PDF (with edits).
    fn selected_source(&self) -> Option<PathBuf> {
        usize::try_from(*self.selected()).ok().and_then(|i| self.rust().queue.get(i)).map(QueueItem::source)
    }

    fn clear_page_text(mut self: Pin<&mut Self>) {
        self.as_mut().set_page_text_json(QString::from("{}"));
        self.as_mut().rust_mut().page_text = None;
        self.as_mut().rust_mut().outlined.clear();
    }

    /// All runs of the page: its text, then the runs recognised in outlines.
    fn runs(&self) -> Vec<pdf2word_core::edit::TextItem> {
        let mut v = self.rust().page_text.as_ref().map(|p| p.items.clone()).unwrap_or_default();
        v.extend(self.rust().outlined.iter().cloned());
        v
    }

    fn set_page_text_value(mut self: Pin<&mut Self>, pt: pdf2word_core::edit::PageText) {
        self.as_mut().rust_mut().page_text = Some(pt);
        self.as_mut().publish_page_text();
    }

    fn publish_page_text(mut self: Pin<&mut Self>) {
        let Some(pt) = self.rust().page_text.clone() else { return };
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let items: Vec<Value> = self
            .runs()
            .iter()
            .enumerate()
            .map(|(i, t)| {
                json!({
                    "i": i, "text": t.text, "x": t.x, "y": t.y, "w": t.w, "h": t.h,
                    "font": t.font, "size": (t.size as f64 * 10.0).round() / 10.0, "bold": t.bold, "italic": t.italic,
                    "color": hex(t.color), "invisible": t.invisible, "rtl": t.rtl, "outlined": t.outlined,
                    "paragraph": if t.outlined { -1 } else { t.paragraph as i64 },
                    "serif": t.font.to_ascii_lowercase().contains("serif") && !t.font.to_ascii_lowercase().contains("sans")
                        || ["times", "roman", "cmr", "georgia", "garamond", "cambria"].iter().any(|k| t.font.to_ascii_lowercase().contains(k)),
                })
            })
            .collect();
        let paras: Vec<Value> = pt
            .paragraphs
            .iter()
            .enumerate()
            .map(|(i, p)| json!({ "i": i, "items": p.items, "text": p.text, "x": p.x, "y": p.y, "w": p.w, "h": p.h, "lines": p.lines,
                "align": format!("{:?}", p.align).to_lowercase() }))
            .collect();
        let images: Vec<Value> = pt.images.iter().enumerate().map(|(i, im)| json!({ "i": i, "x": im.x, "y": im.y, "w": im.w, "h": im.h })).collect();
        let v = json!({ "w": pt.width, "h": pt.height, "outlined": pt.outlined_shapes, "recognised": !self.rust().outlined.is_empty(), "items": items, "paragraphs": paras, "images": images });
        self.as_mut().set_page_text_json(QString::from(&v.to_string()));
    }

    pub fn set_edit_mode_on(mut self: Pin<&mut Self>, on: bool) {
        self.as_mut().set_edit_mode(on);
        self.as_mut().drop_pending();
        if !on {
            self.clear_page_text();
            return;
        }
        let page = *self.preview_page();
        self.show_page(page);
    }

    /// Forgets a preview (and shows the page as it is when it was shown).
    fn drop_pending(mut self: Pin<&mut Self>) {
        if let Some((_, path, _)) = self.as_mut().rust_mut().preview_edit.take() {
            let _ = std::fs::remove_file(path);
        }
        self.as_mut().rust_mut().queued_preview = None;
        if *self.previewing() {
            self.as_mut().set_previewing(false);
        }
    }

    /// Runs an edit of the selected file in the background. With a `preview`
    /// key the result is only shown; otherwise it becomes the file's new version.
    fn run_edit(mut self: Pin<&mut Self>, what: String, job: EditJob, preview: Option<String>) {
        if *self.busy() {
            return;
        }
        if *self.edit_busy() {
            if let Some(key) = preview {
                self.as_mut().rust_mut().queued_preview = Some((key, what, job));
            }
            return;
        }
        let Some(source) = self.selected_source() else { return };
        let input = PathBuf::from(self.input_path().to_string());
        let password = opt_string(self.password());
        let page = *self.preview_page();
        self.as_mut().set_edit_busy(true);
        if preview.is_none() {
            self.as_mut().set_status(QString::from(&format!("{what}…")));
        }
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let n = PREVIEW_COUNTER.fetch_add(1, Ordering::SeqCst);
            let out = std::env::temp_dir().join(format!("pdf2word-edit-{}-{n}.pdf", std::process::id()));
            let result = job(&source, password.as_deref(), &out);
            let png = match (&result, &preview) {
                (Ok(_), Some(_)) => {
                    let png = std::env::temp_dir().join(format!("pdf2word-preview-{}-{n}.png", std::process::id()));
                    pdf2word_core::render_page_png(&out, password.as_deref(), (page - 1).max(0) as usize, 1200, &png, None).ok().map(|_| png)
                }
                _ => None,
            };
            let _ = qt_thread.queue(move |mut obj| {
                obj.as_mut().set_edit_busy(false);
                let index = obj.rust().queue.iter().position(|q| q.input == input);
                let current = obj.selected_source();
                match (result, preview) {
                    (Ok(_), Some(key)) => {
                        // Still the same page of the same version?
                        if current.as_deref() == Some(source.as_path()) && *obj.preview_page() == page && *obj.edit_mode() {
                            if let Some((_, old, _)) = obj.as_mut().rust_mut().preview_edit.replace((key, out, source)) {
                                let _ = std::fs::remove_file(old);
                            }
                            if let Some(png) = png {
                                if let Some(old) = url_to_path(obj.preview_source()) {
                                    let _ = std::fs::remove_file(old);
                                }
                                obj.as_mut().set_preview_source(path_to_url(&png));
                                obj.as_mut().set_previewing(true);
                            }
                        } else {
                            let _ = std::fs::remove_file(&out);
                        }
                    }
                    (Ok((changes, notes)), None) if changes > 0 && index.is_some() => {
                        let index = index.unwrap();
                        obj.as_mut().commit_version(index, out, &what, &notes);
                    }
                    (Ok(_), None) => {
                        let _ = std::fs::remove_file(&out);
                        obj.as_mut().set_status(QString::from(&format!("{what}: nothing changed")));
                    }
                    (Err(e), preview) => {
                        let _ = std::fs::remove_file(&out);
                        obj.as_mut().set_status(QString::from(&format!("{what} failed: {e:#}")));
                        if preview.is_none() {
                            obj.as_mut().append_log(&format!("✖ {what} failed: {e:#}"));
                        }
                    }
                }
                if let Some((key, what, job)) = obj.as_mut().rust_mut().queued_preview.take() {
                    obj.as_mut().run_edit(what, job, Some(key));
                }
            });
        });
    }

    /// Makes `out` the new version of queue item `index`.
    fn commit_version(mut self: Pin<&mut Self>, index: usize, out: PathBuf, what: &str, notes: &[String]) {
        if let Some(item) = self.as_mut().rust_mut().queue.get_mut(index) {
            item.clear_redo();
            // Into the file's session folder, where it outlives this run.
            let kept = session_dir(&item.input)
                .and_then(|dir| {
                    std::fs::create_dir_all(&dir).ok()?;
                    let n = PREVIEW_COUNTER.fetch_add(1, Ordering::SeqCst);
                    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                    let target = dir.join(format!("v{stamp}-{n}.pdf"));
                    (std::fs::rename(&out, &target).is_ok() || (std::fs::copy(&out, &target).is_ok() && std::fs::remove_file(&out).is_ok())).then_some(target)
                })
                .unwrap_or(out);
            item.versions.push(kept);
            item.save_session();
            if item.status == ItemStatus::Done {
                item.status = ItemStatus::Waiting;
                item.progress = 0.0;
            }
        }
        for n in notes {
            self.as_mut().append_log(&format!("  ⚠ {n}"));
        }
        let msg = match notes.first() {
            Some(n) => format!("{what}: done — {n}"),
            None => format!("{what}: done"),
        };
        self.as_mut().set_status(QString::from(&msg));
        self.as_mut().set_previewing(false);
        self.as_mut().sync_queue();
        let goto = self.as_mut().rust_mut().goto_after_edit.take();
        if *self.selected() == index as i32 {
            self.as_mut().refresh_page_count();
            let page = goto.unwrap_or(*self.preview_page());
            self.show_page(page);
        }
    }

    /// Reads the page count of the selected file's current version again.
    fn refresh_page_count(mut self: Pin<&mut Self>) {
        let Some(source) = self.selected_source() else { return };
        let password = opt_string(self.password());
        if let Ok(pdfium) = pdf2word_core::load_pdfium(None) {
            if let Ok(doc) = pdfium.load_pdf_from_file(&source, password.as_deref()) {
                let n = doc.pages().len();
                drop(doc);
                self.as_mut().set_page_count(n);
                let info = self.document_info().to_string();
                if let Some(rest) = info.split_once(" page").map(|(a, b)| (a.rsplit_once(' ').map(|x| x.0).unwrap_or(""), b)) {
                    let prefix = if rest.0.is_empty() { String::new() } else { format!("{} ", rest.0) };
                    let plural = if n == 1 { "" } else { "s" };
                    let tail = rest.1.trim_start_matches('s');
                    self.as_mut().set_document_info(QString::from(&format!("{prefix}{n} page{plural}{tail}")));
                }
            }
        }
    }

    /// Applies `edits` (or shows them, with a preview key).
    fn apply(mut self: Pin<&mut Self>, what: &str, edits: Vec<pdf2word_core::edit::TextEdit>, preview: bool) {
        if edits.is_empty() {
            if preview {
                self.cancel_preview();
            }
            return;
        }
        let key = format!("{edits:?}");
        let source = self.selected_source();
        if !preview {
            // The preview of exactly this edit is ready: keep it.
            let ready = match (&self.rust().preview_edit, &source) {
                (Some((k, path, base)), Some(src)) if *k == key && base == src && path.is_file() => Some(path.clone()),
                _ => None,
            };
            if let Some(path) = ready {
                self.as_mut().rust_mut().preview_edit = None;
                let input = PathBuf::from(self.input_path().to_string());
                if let Some(index) = self.rust().queue.iter().position(|q| q.input == input) {
                    self.commit_version(index, path, what, &[]);
                }
                return;
            }
            self.as_mut().drop_pending();
        }
        self.run_edit(
            what.to_string(),
            Box::new(move |src, pw, out| {
                let r = pdf2word_core::edit::apply_edits(src, pw, &edits, out, None)?;
                Ok((1, r.notes))
            }),
            preview.then_some(key),
        );
    }

    fn page_index(&self) -> usize {
        (*self.preview_page() - 1).max(0) as usize
    }

    /// The edit for run `index` with new text and style.
    fn run_edits(&self, index: i32, text: &QString, fit: bool, style: &QString) -> Vec<pdf2word_core::edit::TextEdit> {
        use pdf2word_core::edit::{NewTextStyle, StyleChange, TextEdit};
        let Some(item) = usize::try_from(index).ok().and_then(|i| self.runs().get(i).cloned()) else { return Vec::new() };
        let text = text.to_string();
        let st: Value = serde_json::from_str(&style.to_string()).unwrap_or(Value::Null);
        let page = self.page_index();
        let color = st.get("color").and_then(Value::as_str).and_then(parse_color);
        let change = StyleChange {
            bold: st.get("bold").and_then(Value::as_bool).filter(|b| *b != item.bold),
            italic: st.get("italic").and_then(Value::as_bool).filter(|b| *b != item.italic),
            size: st.get("size").and_then(Value::as_f64).map(|v| v as f32).filter(|v| (*v - item.size).abs() > 0.05),
            color: color.filter(|c| *c != item.color),
        };
        if item.outlined {
            let family = if item.font.contains("(serif)") {
                "serif"
            } else if item.font.contains("monospace") {
                "mono"
            } else {
                ""
            };
            return vec![TextEdit::Outlined {
                page,
                x: item.x,
                y: item.y,
                w: item.w,
                h: item.h,
                baseline: item.baseline,
                text,
                size: change.size.unwrap_or(item.size),
                color: change.color.unwrap_or(item.color),
                style: NewTextStyle { family: family.into(), bold: change.bold.unwrap_or(item.bold), italic: change.italic.unwrap_or(item.italic) },
                fit,
            }];
        }
        if change != StyleChange::default() {
            let text = (text != item.text).then_some(text);
            return vec![TextEdit::Restyle { page, objects: item.objects, text, change, fit }];
        }
        if text == item.text {
            return Vec::new();
        }
        vec![TextEdit::Replace { page, objects: item.objects, text, fit }]
    }

    pub fn edit_run(self: Pin<&mut Self>, index: i32, text: &QString, fit: bool, style: &QString) {
        let edits = self.run_edits(index, text, fit, style);
        self.apply("Changing text", edits, false);
    }

    pub fn preview_run(self: Pin<&mut Self>, index: i32, text: &QString, fit: bool, style: &QString) {
        let edits = self.run_edits(index, text, fit, style);
        self.apply("Preview", edits, true);
    }

    fn paragraph_edits(&self, index: i32, text: &QString, fit: bool, push_down: bool) -> Vec<pdf2word_core::edit::TextEdit> {
        let Some(pt) = self.rust().page_text.as_ref() else { return Vec::new() };
        let Some(p) = usize::try_from(index).ok().and_then(|i| pt.paragraphs.get(i)) else { return Vec::new() };
        let text = text.to_string();
        if text == p.text {
            return Vec::new();
        }
        let objects = p.items.iter().filter_map(|i| pt.items.get(*i)).flat_map(|i| i.objects.clone()).collect();
        vec![pdf2word_core::edit::TextEdit::Paragraph { page: self.page_index(), objects, text, fit, push_down }]
    }

    pub fn edit_paragraph(self: Pin<&mut Self>, index: i32, text: &QString, fit: bool, push_down: bool) {
        let edits = self.paragraph_edits(index, text, fit, push_down);
        self.apply("Changing the paragraph", edits, false);
    }

    pub fn preview_paragraph(self: Pin<&mut Self>, index: i32, text: &QString, fit: bool, push_down: bool) {
        let edits = self.paragraph_edits(index, text, fit, push_down);
        self.apply("Preview", edits, true);
    }

    pub fn cancel_preview(mut self: Pin<&mut Self>) {
        let was = *self.previewing() || self.rust().preview_edit.is_some();
        self.as_mut().drop_pending();
        if was {
            let page = *self.preview_page();
            self.show_page(page);
        }
    }

    pub fn delete_items(self: Pin<&mut Self>, indices: &QString) {
        use pdf2word_core::edit::{NewTextStyle, TextEdit};
        let idx: Vec<usize> = serde_json::from_str(&indices.to_string()).unwrap_or_default();
        let runs = self.runs();
        let page = self.page_index();
        let edits: Vec<TextEdit> = idx
            .iter()
            .filter_map(|i| runs.get(*i))
            .map(|it| {
                if it.outlined {
                    TextEdit::Outlined {
                        page,
                        x: it.x,
                        y: it.y,
                        w: it.w,
                        h: it.h,
                        baseline: it.baseline,
                        text: String::new(),
                        size: it.size,
                        color: it.color,
                        style: NewTextStyle::default(),
                        fit: false,
                    }
                } else {
                    TextEdit::Delete { page, objects: it.objects.clone() }
                }
            })
            .collect();
        let what = if edits.len() == 1 { "Deleting text".to_string() } else { format!("Deleting {} lines", edits.len()) };
        self.apply(&what, edits, false);
    }

    pub fn move_text(self: Pin<&mut Self>, index: i32, dx: f64, dy: f64) {
        let Some(item) = usize::try_from(index).ok().and_then(|i| self.runs().get(i).cloned()) else { return };
        if item.outlined || (dx.abs() < 1e-4 && dy.abs() < 1e-4) {
            return;
        }
        let edit = pdf2word_core::edit::TextEdit::Move { page: self.page_index(), objects: item.objects, dx: dx as f32, dy: dy as f32 };
        self.apply("Moving text", vec![edit], false);
    }

    pub fn insert_text(self: Pin<&mut Self>, x: f64, y: f64, text: &QString, size: f64, style: i32) {
        let (family, bold, italic) = (style & 3, style & 4 != 0, style & 8 != 0);
        let text = text.to_string();
        if text.trim().is_empty() {
            return;
        }
        let family = match family {
            1 => "serif",
            2 => "mono",
            _ => "",
        }
        .to_string();
        let edit = pdf2word_core::edit::TextEdit::Insert {
            page: self.page_index(),
            x: x as f32,
            y: y as f32,
            text,
            size: size.clamp(2.0, 400.0) as f32,
            color: [0, 0, 0],
            style: pdf2word_core::edit::NewTextStyle { family, bold, italic },
        };
        self.apply("Adding text", vec![edit], false);
    }

    pub fn find_text(mut self: Pin<&mut Self>, query: &QString, match_case: bool, whole_word: bool) {
        let query = query.to_string();
        let Some(source) = self.selected_source() else { return };
        if query.is_empty() || *self.find_busy() {
            return;
        }
        let password = opt_string(self.password());
        self.as_mut().set_find_busy(true);
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let opts = pdf2word_core::edit::FindOptions { match_case, whole_word };
            let result = pdf2word_core::edit::find_text(&source, password.as_deref(), &query, opts, None);
            let _ = qt_thread.queue(move |mut obj| {
                obj.as_mut().set_find_busy(false);
                match result {
                    Ok(matches) => {
                        let v: Vec<Value> = matches
                            .iter()
                            .enumerate()
                            .map(|(i, m)| json!({ "i": i, "page": m.page + 1, "before": m.before, "found": m.found, "after": m.after }))
                            .collect();
                        obj.as_mut().set_status(QString::from(&format!("{} match{} found", matches.len(), if matches.len() == 1 { "" } else { "es" })));
                        obj.as_mut().rust_mut().matches = matches;
                        obj.as_mut().rust_mut().matches_source = source;
                        obj.as_mut().set_find_json(QString::from(&Value::Array(v).to_string()));
                    }
                    Err(e) => obj.as_mut().set_status(QString::from(&format!("Search failed: {e:#}"))),
                }
            });
        });
    }

    pub fn replace_found(mut self: Pin<&mut Self>, indices: &QString, replacement: &QString) {
        let idx: Vec<usize> = serde_json::from_str(&indices.to_string()).unwrap_or_default();
        let replacement = replacement.to_string();
        if self.selected_source().as_ref() != Some(&self.rust().matches_source) {
            self.as_mut().set_status(QString::from("The document changed: search again"));
            return;
        }
        let chosen: Vec<pdf2word_core::edit::Match> = idx.iter().filter_map(|i| self.rust().matches.get(*i).cloned()).collect();
        if chosen.is_empty() {
            return;
        }
        let n = chosen.len();
        self.as_mut().set_find_json(QString::from("[]"));
        self.as_mut().rust_mut().matches.clear();
        self.as_mut().drop_pending();
        self.run_edit(
            format!("Replacing {n} match{}", if n == 1 { "" } else { "es" }),
            Box::new(move |src, pw, out| {
                let r = pdf2word_core::edit::replace_matches(src, pw, &chosen, &replacement, out, None)?;
                Ok((n, r.notes))
            }),
            None,
        );
    }

    pub fn recognize_outlined(mut self: Pin<&mut Self>) {
        let Some(source) = self.selected_source() else { return };
        if *self.ocr_busy() {
            return;
        }
        let input = PathBuf::from(self.input_path().to_string());
        let page = *self.preview_page();
        let password = opt_string(self.password());
        let language = opt_string(self.ocr_language()).unwrap_or_else(|| "auto".into());
        self.as_mut().rust_mut().ocr_pages.insert((input.clone(), page));
        self.as_mut().set_ocr_busy(true);
        self.as_mut().set_status(QString::from("Recognising outlined text…"));
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            let result = pdf2word_core::edit::outlined_text(&source, password.as_deref(), (page - 1).max(0) as usize, &language, None);
            let _ = qt_thread.queue(move |mut obj| {
                obj.as_mut().set_ocr_busy(false);
                if *obj.preview_page() != page || obj.input_path().to_string() != input.to_string_lossy() {
                    return;
                }
                match result {
                    Ok(items) => {
                        let n = items.len();
                        obj.as_mut().rust_mut().outlined = items;
                        obj.as_mut().publish_page_text();
                        obj.as_mut().set_status(QString::from(&format!("{n} line{} of outlined text recognised", if n == 1 { "" } else { "s" })));
                    }
                    Err(e) => obj.as_mut().set_status(QString::from(&format!("Could not recognise the outlined text: {e:#}"))),
                }
            });
        });
    }

    pub fn undo_edit(mut self: Pin<&mut Self>) {
        if *self.edit_busy() || *self.busy() {
            return;
        }
        self.as_mut().drop_pending();
        let Ok(i) = usize::try_from(*self.selected()) else { return };
        let undone = match self.as_mut().rust_mut().queue.get_mut(i) {
            Some(item) => match item.versions.pop() {
                Some(v) => {
                    item.redo.push(v);
                    item.save_session();
                    if item.status == ItemStatus::Done {
                        item.status = ItemStatus::Waiting;
                        item.progress = 0.0;
                    }
                    true
                }
                None => false,
            },
            None => false,
        };
        if undone {
            self.as_mut().set_status(QString::from("Edit undone"));
            self.as_mut().sync_queue();
            self.as_mut().refresh_page_count();
            let page = *self.preview_page();
            self.show_page(page);
        }
    }

    pub fn redo_edit(mut self: Pin<&mut Self>) {
        if *self.edit_busy() || *self.busy() {
            return;
        }
        self.as_mut().drop_pending();
        let Ok(i) = usize::try_from(*self.selected()) else { return };
        let redone = match self.as_mut().rust_mut().queue.get_mut(i) {
            Some(item) => match item.redo.pop() {
                Some(v) => {
                    item.versions.push(v);
                    item.save_session();
                    true
                }
                None => false,
            },
            None => false,
        };
        if redone {
            self.as_mut().set_status(QString::from("Edit redone"));
            self.as_mut().sync_queue();
            self.as_mut().refresh_page_count();
            let page = *self.preview_page();
            self.show_page(page);
        }
    }

    pub fn save_edited_pdf(mut self: Pin<&mut Self>, url: &QUrl) {
        let Some(mut path) = url_to_path(url) else { return };
        if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("pdf")) {
            path.set_extension("pdf");
        }
        let Some(source) = self.selected_source() else { return };
        if path == source {
            self.as_mut().set_status(QString::from("Choose another file name"));
            return;
        }
        let password = opt_string(self.password());
        self.as_mut().set_status(QString::from("Saving the PDF…"));
        let qt_thread = self.qt_thread();
        std::thread::spawn(move || {
            // Without the earlier revisions: removed text is really gone.
            let result = pdf2word_core::edit::compact(&source, password.as_deref(), &path, None).or_else(|_| std::fs::copy(&source, &path).map(|_| ()).map_err(anyhow::Error::from));
            let _ = qt_thread.queue(move |mut obj| {
                let msg = match result {
                    Ok(()) => {
                        if let Some(q) = obj.as_mut().rust_mut().queue.iter_mut().find(|q| q.versions.last() == Some(&source)) {
                            q.saved = Some(source.clone());
                        }
                        format!("Saved {}", path.display())
                    }
                    Err(e) => format!("Could not save {}: {e:#}", path.display()),
                };
                obj.as_mut().append_log(&msg);
                obj.as_mut().set_status(QString::from(&msg));
            });
        });
    }

    fn page_ops(self: Pin<&mut Self>, what: &str, ops: Vec<pdf2word_core::pages::PageOp>) {
        self.run_edit(
            what.to_string(),
            Box::new(move |src, pw, out| {
                pdf2word_core::pages::apply_page_ops(src, pw, &ops, out)?;
                Ok((1, Vec::new()))
            }),
            None,
        );
    }

    pub fn page_op(mut self: Pin<&mut Self>, kind: &QString) {
        use pdf2word_core::pages::PageOp;
        let page = self.page_index();
        let count = (*self.page_count()).max(0) as usize;
        let (what, op, next) = match kind.to_string().as_str() {
            "rotate-left" => ("Turning the page", PageOp::Rotate { pages: vec![page], degrees: -90 }, page),
            "rotate-right" => ("Turning the page", PageOp::Rotate { pages: vec![page], degrees: 90 }, page),
            "delete" if count > 1 => ("Deleting the page", PageOp::Delete { pages: vec![page] }, page.min(count - 2)),
            "move-earlier" if page > 0 => ("Moving the page", PageOp::Move { from: page, to: page - 1 }, page - 1),
            "move-later" if page + 1 < count => ("Moving the page", PageOp::Move { from: page, to: page + 1 }, page + 1),
            "insert-blank" => ("Inserting a page", PageOp::InsertBlank { at: page + 1 }, page + 1),
            _ => return,
        };
        self.as_mut().rust_mut().goto_after_edit = Some(next as i32 + 1);
        self.page_ops(what, vec![op]);
    }

    pub fn insert_pdf(mut self: Pin<&mut Self>, url: &QUrl) {
        let Some(file) = url_to_path(url) else { return };
        let at = self.page_index() + 1;
        self.as_mut().rust_mut().goto_after_edit = Some(at as i32 + 1);
        let password = opt_string(self.password());
        self.page_ops("Inserting pages", vec![pdf2word_core::pages::PageOp::InsertPdf { at, file, password, pages: None }]);
    }

    pub fn extract_pages(mut self: Pin<&mut Self>, spec: &QString, url: &QUrl) {
        let Some(mut out) = url_to_path(url) else { return };
        if out.extension().is_none_or(|e| !e.eq_ignore_ascii_case("pdf")) {
            out.set_extension("pdf");
        }
        let Some(source) = self.selected_source() else { return };
        let password = opt_string(self.password());
        let count = (*self.page_count()).max(0) as usize;
        let msg = match pdf2word_core::parse_page_range(&spec.to_string(), count)
            .and_then(|pages| pdf2word_core::pages::extract(&source, password.as_deref(), &pages, &out).map(|_| pages.len()))
        {
            Ok(n) => format!("Saved {n} page{} as {}", if n == 1 { "" } else { "s" }, out.display()),
            Err(e) => format!("Could not extract the pages: {e:#}"),
        };
        self.as_mut().append_log(&msg);
        self.as_mut().set_status(QString::from(&msg));
    }

    pub fn split_pdf(mut self: Pin<&mut Self>, every: i32, folder: &QUrl) {
        let Some(dir) = url_to_path(folder).filter(|p| p.is_dir()) else { return };
        let Some(source) = self.selected_source() else { return };
        let input = PathBuf::from(self.input_path().to_string());
        let password = opt_string(self.password());
        // Name the parts after the original file, not the edited copy.
        let tmp = std::env::temp_dir().join(format!("pdf2word-split-{}", std::process::id()));
        let named = tmp.join(input.file_name().unwrap_or_default());
        let result = std::fs::create_dir_all(&tmp)
            .map_err(anyhow::Error::from)
            .and_then(|_| std::fs::copy(&source, &named).map_err(anyhow::Error::from))
            .and_then(|_| pdf2word_core::pages::split(&named, password.as_deref(), every.max(1) as usize, &dir));
        let _ = std::fs::remove_dir_all(&tmp);
        let msg = match result {
            Ok(parts) => format!("Split into {} file{} in {}", parts.len(), if parts.len() == 1 { "" } else { "s" }, dir.display()),
            Err(e) => format!("Could not split the PDF: {e:#}"),
        };
        self.as_mut().append_log(&msg);
        self.as_mut().set_status(QString::from(&msg));
    }

    pub fn merge_files(mut self: Pin<&mut Self>, url: &QUrl) {
        let Some(mut out) = url_to_path(url) else { return };
        if out.extension().is_none_or(|e| !e.eq_ignore_ascii_case("pdf")) {
            out.set_extension("pdf");
        }
        let password = opt_string(self.password());
        let inputs: Vec<(PathBuf, Option<String>)> = self.rust().queue.iter().map(|q| (q.source(), password.clone())).collect();
        if inputs.len() < 2 {
            return;
        }
        match pdf2word_core::pages::merge(&inputs, &out) {
            Ok(()) => {
                let msg = format!("Joined {} files into {}", inputs.len(), out.display());
                self.as_mut().append_log(&msg);
                let i = self.as_mut().add_path(out);
                self.as_mut().sync_queue();
                self.as_mut().select_item(i as i32);
                self.as_mut().set_status(QString::from(&msg));
            }
            Err(e) => self.as_mut().set_status(QString::from(&format!("Could not join the files: {e:#}"))),
        }
    }

    pub fn redact_area(self: Pin<&mut Self>, x: f64, y: f64, w: f64, h: f64) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let edit = pdf2word_core::edit::TextEdit::Redact { page: self.page_index(), x: x as f32, y: y as f32, w: w as f32, h: h as f32, fill: None };
        self.apply("Redacting", vec![edit], false);
    }

    pub fn redact_found(mut self: Pin<&mut Self>, indices: &QString) {
        let idx: Vec<usize> = serde_json::from_str(&indices.to_string()).unwrap_or_default();
        if self.selected_source().as_ref() != Some(&self.rust().matches_source) {
            self.as_mut().set_status(QString::from("The document changed: search again"));
            return;
        }
        let chosen: Vec<pdf2word_core::edit::Match> = idx.iter().filter_map(|i| self.rust().matches.get(*i).cloned()).collect();
        if chosen.is_empty() {
            return;
        }
        let n = chosen.len();
        self.as_mut().set_find_json(QString::from("[]"));
        self.as_mut().rust_mut().matches.clear();
        self.as_mut().drop_pending();
        self.run_edit(
            format!("Redacting {n} match{}", if n == 1 { "" } else { "es" }),
            Box::new(move |src, pw, out| {
                let r = pdf2word_core::edit::redact_matches(src, pw, &chosen, None, out, None)?;
                Ok((n, r.notes))
            }),
            None,
        );
    }

    fn image_path(&self, index: i32) -> Option<Vec<usize>> {
        let pt = self.rust().page_text.as_ref()?;
        usize::try_from(index).ok().and_then(|i| pt.images.get(i)).map(|im| im.path.clone())
    }

    pub fn delete_image(self: Pin<&mut Self>, index: i32) {
        let Some(path) = self.image_path(index) else { return };
        let edit = pdf2word_core::edit::TextEdit::Image { page: self.page_index(), path, change: pdf2word_core::edit::ImageChange::Delete };
        self.apply("Deleting the picture", vec![edit], false);
    }

    pub fn place_image(self: Pin<&mut Self>, index: i32, x: f64, y: f64, w: f64, h: f64) {
        let Some(path) = self.image_path(index) else { return };
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let change = pdf2word_core::edit::ImageChange::Place { x: x as f32, y: y as f32, w: w as f32, h: h as f32 };
        let edit = pdf2word_core::edit::TextEdit::Image { page: self.page_index(), path, change };
        self.apply("Placing the picture", vec![edit], false);
    }

    pub fn replace_image(self: Pin<&mut Self>, index: i32, url: &QUrl) {
        let (Some(path), Some(file)) = (self.image_path(index), url_to_path(url)) else { return };
        let edit = pdf2word_core::edit::TextEdit::Image { page: self.page_index(), path, change: pdf2word_core::edit::ImageChange::Replace { file } };
        self.apply("Replacing the picture", vec![edit], false);
    }

    pub fn insert_image(mut self: Pin<&mut Self>, url: &QUrl) {
        let Some(file) = url_to_path(url) else { return };
        let Some((iw, ih)) = pdf2word_core::edit::image_size(&file) else {
            self.as_mut().set_status(QString::from("This picture cannot be read"));
            return;
        };
        // A third of the page wide (or less tall than half of it), centred.
        let (pw, ph) = self.rust().page_text.as_ref().map(|p| (p.width as f64, p.height as f64)).unwrap_or((612.0, 792.0));
        let mut w = pw / 3.0;
        let mut h = w * ih as f64 / iw.max(1) as f64;
        if h > ph / 2.0 {
            h = ph / 2.0;
            w = h * iw as f64 / ih.max(1) as f64;
        }
        let (fw, fh) = (w / pw, h / ph);
        let edit = pdf2word_core::edit::TextEdit::InsertImage { page: self.page_index(), x: (0.5 - fw / 2.0) as f32, y: (0.5 - fh / 2.0) as f32, w: fw as f32, h: fh as f32, file };
        self.apply("Adding the picture", vec![edit], false);
    }

    pub fn unsaved_files(&self) -> i32 {
        self.rust().queue.iter().filter(|q| !q.versions.is_empty() && q.saved.as_ref() != q.versions.last()).count() as i32
    }

    pub fn save_all_edited(mut self: Pin<&mut Self>, folder: &QUrl) {
        let Some(dir) = url_to_path(folder).filter(|p| p.is_dir()) else { return };
        let password = opt_string(self.password());
        let mut saved = 0;
        let mut failed = Vec::new();
        let items: Vec<(usize, PathBuf, PathBuf)> = self
            .rust()
            .queue
            .iter()
            .enumerate()
            .filter(|(_, q)| !q.versions.is_empty())
            .map(|(i, q)| (i, q.input.clone(), q.source()))
            .collect();
        for (i, input, source) in items {
            let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "document".into());
            let target = dir.join(format!("{stem}-edited.pdf"));
            match pdf2word_core::edit::compact(&source, password.as_deref(), &target, None) {
                Ok(()) => {
                    saved += 1;
                    if let Some(q) = self.as_mut().rust_mut().queue.get_mut(i) {
                        q.saved = Some(source);
                    }
                }
                Err(e) => failed.push(format!("{}: {e:#}", input.display())),
            }
        }
        for f in &failed {
            self.as_mut().append_log(&format!("✖ {f}"));
        }
        let msg = format!("{saved} edited PDF{} saved in {}", if saved == 1 { "" } else { "s" }, dir.display());
        self.as_mut().append_log(&msg);
        self.as_mut().set_status(QString::from(&msg));
    }

    pub fn discard_all_edits(mut self: Pin<&mut Self>) {
        for q in self.as_mut().rust_mut().queue.iter_mut() {
            q.discard_versions();
        }
        self.as_mut().sync_queue();
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
