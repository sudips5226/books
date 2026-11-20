# pdf2word — PDF to Word converter (Rust · PDFium · Qt · OCR)

Converts PDF documents to editable Word (`.docx`) files while keeping the
layout and formatting of the original, including scanned pages through OCR.

* **Rust** conversion engine (`pdf2word-core`), command-line tool (`pdf2word`)
  and a **Qt 6 / QML** desktop application (`pdf2word-gui`, built with [CXX-Qt](https://kdab.github.io/cxx-qt/)).
* **PDFium** (via [`pdfium-render`](https://crates.io/crates/pdfium-render)) parses and renders PDFs.
* **Tesseract** OCR for scanned pages, linked through its stable C API.
* A purpose-built WordprocessingML writer: no Office, LibreOffice or other runtime needed.
* **Text editing in the PDF itself**: correct a word, delete or add text, or
  find and replace throughout the document, then save the corrected PDF (or
  convert it to Word).

## What is preserved

| Area | Details |
|---|---|
| Characters | font family (PostScript names mapped to Word fonts), size, bold, italic, small caps, colour, underline, strike-through, highlight, super/subscripts with exact baseline shift, hyperlinks, ligatures expanded |
| Fonts | the PDF's own TrueType fonts are **embedded** into the DOCX when their licence allows (subset fonts get a rebuilt Unicode map); otherwise each run gets **character spacing / scaling** so that it keeps its PDF width with the font Word substitutes (built-in metrics of Word's core fonts plus installed system fonts) |
| Paragraphs | left/centre/right/justified alignment, left/right indents, first-line and hanging indents, exact line pitch, original vertical spacing, tab stops (including right tabs for page numbers), hyphenation joined in flowing text, intentional line breaks (addresses, titles) kept |
| Styles | **paragraph styles** built from the document: Normal from the body text, Heading 1–6, Caption, Footnote Text, TOC 1–3 and recurring formats; runs only carry what differs from their style, so restyling the document in Word works |
| Structure | multi-column layouts (recursive XY-cut, reading order preserved); **Word lists** (bullets, numbers, letters, roman numerals, nested levels) instead of typed markers; **headers and footers** from repeated page tops/bottoms, with **PAGE/NUMPAGES fields**, a distinct first page, even/odd pages of two-sided layouts, roman-numbered front matter and chapter openings without header; **footnotes** whose markers appear in the text |
| Navigation | the **PDF outline** sets heading levels and bookmarks (Word's navigation pane mirrors it); **links inside the document** become hyperlinks to bookmarks; **tables of contents** become TOC paragraphs with dot leaders, links to the headings and a TOC field Word can update |
| Review & forms | sticky notes and commented markup become **Word comments** (author, date); highlight, underline and strike-out annotations format the text they cover; ink, shapes and stamps are kept as transparent pictures over the page; **form fields** (text, check boxes, radio buttons, lists) become **content controls** with their values |
| Scripts | **right-to-left** Hebrew and Arabic (visual → logical order, mirrored brackets, Arabic presentation forms normalised, bidi paragraphs and tables); **rotated text** and **vertical East Asian text** as positioned text boxes |
| Equations | maths fonts (TeX, Cambria Math, STIX …) become **native Word equations (OMML)**: fractions, radicals, sums/integrals with limits, super/subscripts, inline and display; matrices and case distinctions are kept as pictures |
| Tables | ruled tables with merged cells (row/column spans), per-edge borders, cell shading; **booktabs tables** (horizontal rules only); whitespace-aligned tables (invoices, statements) with per-column alignment; a distinct first row repeats as **header row** on every page |
| Graphics | embedded raster images (PNG/JPEG); **simple vector charts and diagrams as editable shapes and text boxes** (with the picture as fallback), other figures rasterised with their labels; background images/watermarks behind text; pictures with text flowing beside them; text inside pictures becomes their **alternative text** |
| Pages | page size and orientation per page, margins derived from content, one section per page so pages stay aligned (or one continuous flow, see below) |
| Scans | Tesseract OCR with per-letter boxes and baselines, **several pages recognised in parallel**; scans are **straightened** (skew) and **turned upright** (sideways or upside-down pages); the OCR **language is detected** from the document; **bold** (stroke width), **italic** (slant) and **font class** (serif / sans / monospace → Times New Roman / Arial / Courier New) from the letter shapes; colour, bullet repair, **table rulings and underlines recovered from pixels**, pictures cropped from the scan; PDFs that already carry an invisible OCR text layer reuse it |

Three layout modes:

* **Flow** (default): real, editable paragraphs on the PDF's pages; columns
  become borderless tables and text reflows naturally when edited.
* **Continuous** (`-m continuous`): one flowing document. Sections only start
  where the page size or the number of text columns changes, text columns
  become **Word columns**, and paragraphs and tables that the PDF broke
  across pages or columns are **joined** again (a repeated table header row is
  dropped and becomes a Word header row). Best for reusing the text.
* **Exact**: every text block is a frame (`w:framePr`) at its absolute page
  position, with pictures and tables anchored there too. This gives the highest
  visual fidelity and suits forms and complex layouts.

## Architecture

```
            ┌──────────────┐   glyphs, fonts, colours, links,
 PDF ──────▶│  extract.rs  │── images, rules, filled boxes,         ┐
  PDFium    │  annots.rs   │   figures (raster + shapes), comments,  │
            └──────────────┘   form fields, annotation overlays      │
                │ scanned page?                                      │  RawPage
                ▼                                                    │
            ┌──────────────┐   straighten, words → pseudo-glyphs,    │
            │ scan.rs      │── rulings and figures from the bitmap   ┘
  Tesseract │ ocr.rs       │   (worker threads)
            └──────────────┘
                                         ┌──────────────┐    PageLayout     ┌──────────────┐
                                RawPage ▶│  layout.rs   │──────────────────▶│ structure.rs │
                                         └──────────────┘  paragraphs,      │ navigate.rs  │
                                 segments → XY-cut tree →  tables, lists,   └──────────────┘
                                 lines → paragraphs/tables pictures, rules   headers/footers, lists,
                                                                             footnotes, styles, outline,
                                                                             links, TOC, comments
                                                                                    │
                                                          reflow.rs (continuous) ───▶ docx.rs ──▶ .docx
```

* `crates/core`: the library (`convert()`, `inspect()`, `render_page_png()`, `page_structure()`, progress events, cancellation;
  `edit::text_items()`, `edit::apply_edits()`, `edit::find_replace()` for editing PDFs).
  Modules: `extract` (PDFium), `annots` (outline, link targets, annotations, form fields),
  `ocr` and `scan` (Tesseract, straightening, language), `layout` (reconstruction, bidi, text boxes, tables),
  `structure` (headers/footers, lists, footnotes, styles), `navigate` (outline, links, TOC, comments, controls),
  `reflow` (continuous mode), `math` (equations → OMML), `metrics` (font widths, compensation),
  `embed` (font embedding), `docx` (writer), `edit` (PDF text editing)
* `crates/cli`: `pdf2word` command-line front-end
* `crates/gui`: Qt Quick application (`src/converter.rs` QObject bridge + `qml/Main.qml`)
* `tools/`: `fetch-pdfium.sh` (prebuilt PDFium download), `make_test_pdfs.py` and `math-fixture.tex`
  (sample documents), `eval.py` (quality harness), `gen_metrics.py` (built-in font metrics), `make_icons.py` (application icon)
* `packaging/`: icons, Windows resource and Inno Setup script, macOS `Info.plist`, Linux `.desktop`

## Downloads

Every push builds ready-to-run packages in GitHub Actions (workflow
**Release builds**); tagged versions (`v*`) publish them as a GitHub release:

| Platform | Package |
|---|---|
| Windows x64 | `pdf2word-setup-<version>-x64.exe` installer (Start menu, optional desktop icon, *Open with* for PDFs) or the portable folder |
| macOS (Apple Silicon) | `pdf2word-<version>-macos-arm64.dmg` |
| macOS (Intel) | `pdf2word-<version>-macos-x86_64.dmg` |
| Linux x86_64 | `pdf2word-<version>-linux-x86_64.AppImage` |

All packages include PDFium, Tesseract and OCR data for English, German,
French, Spanish, Italian, Portuguese and Dutch.

**Signing.** The workflow signs the programs when the repository has these secrets:

* Windows: `WINDOWS_CERTIFICATE` (base64 of a code-signing `.pfx`) and
  `WINDOWS_CERTIFICATE_PASSWORD`; the programs and the installer are signed
  with `signtool` and timestamped.
* macOS: `MACOS_CERTIFICATE` (base64 of a *Developer ID Application* `.p12`),
  `MACOS_CERTIFICATE_PASSWORD` and `MACOS_SIGNING_IDENTITY`; the app is signed
  with the hardened runtime. With `APPLE_ID`, `APPLE_TEAM_ID` and
  `APPLE_APP_PASSWORD` (an app-specific password) the disk image is also
  notarised and stapled.

Without them the builds are unsigned (macOS: ad-hoc signed), so Windows
SmartScreen and macOS Gatekeeper warn on first start (macOS: right-click → Open).

## Building

Requirements:

* Rust 1.88+
* PDFium shared library at **runtime** (not needed to compile): `tools/fetch-pdfium.sh` downloads build 7350 from
  [pdfium-binaries](https://github.com/bblanchon/pdfium-binaries)
* Tesseract 4/5 development files for OCR (`ocr` feature, on by default)
* Qt 6.4+ (Core, Gui, Qml, Quick, QuickControls2) for the GUI only

### Linux (Debian/Ubuntu)

```sh
sudo apt install libtesseract-dev libleptonica-dev tesseract-ocr-eng \
                 qt6-base-dev qt6-declarative-dev qml6-module-qtquick-controls \
                 qml6-module-qtquick-layouts qml6-module-qtquick-dialogs qml6-module-qtquick-templates \
                 qml6-module-qtquick-window qml6-module-qtqml-workerscript
tools/fetch-pdfium.sh ./pdfium

cargo build --release                          # core + CLI
QMAKE=qmake6 cargo build --release -p pdf2word-gui
```

### macOS

```sh
brew install tesseract qt@6
tools/fetch-pdfium.sh ./pdfium
cargo build --release
QMAKE=$(brew --prefix qt@6)/bin/qmake cargo build --release -p pdf2word-gui
```

### Windows

Install Qt 6 (online installer, MSVC kit) and Tesseract via vcpkg
(`vcpkg install tesseract:x64-windows`), then:

```bat
set TESSERACT_LIB_DIR=C:\vcpkg\installed\x64-windows\lib
rem Use the name of the tesseract*.lib file that vcpkg installed:
set TESSERACT_LIB_NAME=tesseract53
set QMAKE=C:\Qt\6.x.x\msvc2022_64\bin\qmake.exe
bash tools/fetch-pdfium.sh pdfium win-x64
cargo build --release -p pdf2word -p pdf2word-gui
```

Copy `pdfium.dll` next to the executables (and run `windeployqt` on `pdf2word-gui.exe`).

To build without OCR, use `--no-default-features`, which needs no Tesseract.

### Where PDFium is looked up

1. `--pdfium <path>` (CLI)
2. `PDFIUM_DYNAMIC_LIB_PATH` (file or directory)
3. the executable's directory, `./lib`, `../lib`, `../Frameworks`
4. the current directory
5. the system library path

## Usage

### Desktop app

```sh
PDFIUM_DYNAMIC_LIB_PATH=./pdfium target/release/pdf2word-gui [file.pdf …]
```

Open or drop one or several PDFs: they are listed under **Files** and
converted one after the other with the same options (**Convert N files**);
each shows its progress and result, and its document can be opened from
there. The Word documents go next to each PDF or into a chosen **output
folder**. Page through the preview of the selected file; **Show detected
structure** draws the headings, paragraphs, lists, tables, columns,
pictures, editable figures and comments the converter finds on the page.
Choose the layout mode and the content, structure and OCR options; they are
remembered for the next start. Conversions run in the background and can be
cancelled.

**Edit text** outlines the page's lines of text: click one to change or
delete it, or click empty space to add text (font family, size, bold,
italic). **Find and replace…** changes a word everywhere in the document,
**Undo** takes edits back one by one and **Save PDF…** writes the corrected
PDF. A file with edits shows ✎ in the list, and converting it uses the
edited version.

### Editing the text of a PDF

Only the edited text changes: the page's content stream is rewritten at the
operators that draw that text, so every other line keeps its exact
position, kerning and font. For a change inside a line, the unchanged
start and end of the line are kept as they were too.

* New text keeps the **original font** when the PDF is known to contain the
  needed letters. PDFs usually embed only the letters they use, so this is
  learned from the characters the font draws anywhere in the document; fonts
  that are not embedded can show any letter of their encoding.
* Letters the font lacks are set in a **similar installed font** (the same
  family if installed, then a metric-compatible clone such as Liberation
  Sans for Arial, then a generic serif/sans/monospace font, then any font
  with the letters), which is embedded into the PDF. The app says when this
  happens.
* **Keep within the original width** condenses longer text (down to half
  its width) so it does not run into what follows. Otherwise text after the
  edit on the same line moves along, as in a word processor.
* Text in OCR text layers (invisible text) can be corrected the same way.

### Command line

```sh
pdf2word report.pdf                         # → report.docx
pdf2word report.pdf -o out.docx -m exact    # positioned frames
pdf2word book.pdf -m continuous             # one flowing document, Word columns
pdf2word scan.pdf --ocr always -l eng+deu   # force OCR, several languages
pdf2word a.pdf b.pdf c.pdf -o converted/    # batch
pdf2word big.pdf -p 1-5,10-                 # page selection
pdf2word in.pdf --replace "2023=2024" --replace "Acme=Acme Ltd" -o fixed.pdf
                                            # correct the PDF's text (no conversion)
```

Run `pdf2word --help` for all options. Content: `--no-images`, `--no-figures`,
`--no-shapes`, `--no-tables`, `--no-headings`, `--no-embed-fonts`,
`--no-width-compensation`. Structure: `--no-headers-footers`, `--no-lists`,
`--no-footnotes`, `--no-styles`, `--no-navigation`, `--no-comments`. OCR:
`--ocr auto|always|never`, `-l/--lang` (`auto` by default: detected from the
document among the installed languages), `--ocr-dpi`, `--ocr-font`,
`--tessdata`, `--no-straighten`, `--no-picture-text`, `--threads`. Also
`--password` and `--pdfium`. With `--replace OLD=NEW` (repeatable) the PDF's
text is corrected and a PDF is written instead (default `<input>-edited.pdf`).

### Library

```rust
use pdf2word_core::{convert, ConvertOptions, LayoutMode};

let opts = ConvertOptions { layout_mode: LayoutMode::Flow, ..Default::default() };
let report = convert("in.pdf".as_ref(), "out.docx".as_ref(), &opts, None, &mut |p| println!("{p:?}"))?;
println!("{} pages, {} tables", report.pages, report.tables);
```

## Testing

```sh
PDFIUM_DYNAMIC_LIB_PATH=./pdfium cargo test
```

Unit tests cover geometry, font names and metrics, width compensation,
ToUnicode parsing, font rebuilding and obfuscation, bidi reordering, list
markers, hyphenation, paragraph and column detection, the equation parser,
skew estimation, language detection, table-of-contents lines, paragraph and
table joining and the DOCX writer. Integration tests
(`crates/core/tests/convert.rs`) convert the sample PDFs in
`crates/core/tests/fixtures`. Regenerate them with
`python3 tools/make_test_pdfs.py DIR`, which needs `reportlab`, `pypdf`,
`fonttools`, `python-bidi` and `arabic-reshaper`. `math.pdf` comes from
`pdflatex tools/math-fixture.tex`. The integration tests check:

* headings, merged cells, formatting, links, lists, reading order, alignment;
* headers/footers with page fields and a blank first page, nested Word lists, footnotes, paragraph styles;
* booktabs tables, repeated header rows, and continuous mode joining pages, columns and tables;
* outline headings and bookmarks, the TOC field and links, comments, highlights, content controls, editable chart shapes;
* exact-mode frames and page selection;
* width compensation for condensed and letter-spaced text;
* the embedded font: it must parse, carry the right family name and map every character;
* right-to-left order with numbers and Latin runs;
* rotated and vertical text boxes;
* equations (fractions, radicals, n-ary operators, scripts) and the matrix pictures;
* OCR: table rulings from pixels, bold/italic/font class, a sideways and skewed scan
  turned upright, and parallel OCR with automatic language.

Without PDFium (or Tesseract language data), those tests are skipped.

`cargo run -p pdf2word-core --example dump_layout -- file.pdf [page]` prints
the analysed layout tree (`DUMP_GLYPHS=1` adds glyphs, rules and rotated text).

### Conversion quality

`tools/eval.py` measures conversion quality on real PDFs (`eval/corpus.json`
plus the fixtures): each document is converted in flow and exact mode, the
DOCX is rendered back to PDF with LibreOffice and compared with the original
(rendered ink, words seen on the pages, reading order, page count, line breaks).

```sh
python3 tools/eval.py fetch                                 # download the corpus
python3 tools/eval.py run --bin target/release/pdf2word     # report in eval/report
python3 tools/eval.py compare eval/report/results.json eval/baseline.json
```

The CI runs it on every push and reports drops against `eval/baseline.json`
(informational, since LibreOffice versions differ between machines).

## Limitations

* **Fonts.** Embedding needs a TrueType font in the PDF with a usable encoding.
  Type 1 and CFF fonts (common in LaTeX and print PDFs) cannot be embedded in
  DOCX, so width compensation is used for them instead. Embedded subsets only
  contain the characters of the document, so text typed later in Word falls
  back to another font. LibreOffice applies kerning by default, so line breaks
  can differ slightly there. Word does not kern unless told to.
* **Headers and footers** are taken from one representative page (two for
  two-sided layouts). Running titles that change from chapter to chapter stay
  in the page body, and a footer is moved towards the page edge where it would
  otherwise take room the body text needs.
* **Continuous mode** joins paragraphs on textual evidence (no sentence end,
  lower-case continuation); floating pictures are anchored to the nearest
  paragraph, so they move with the text.
* **Rotated and vertical text** use standard DrawingML text boxes.
  LibreOffice ignores shape rotation (30° text shows horizontally) and turns
  vertical CJK glyphs sideways. This has not been checked in Word.
* **Equations** are rebuilt from geometry. Matrices, case distinctions and
  other two-dimensional layouts are kept as pictures, not editable maths.
  Multi-line aligned equations become one equation per line.
* **Figures** become editable shapes only when they consist of paths and
  horizontal labels (no images, shadings or rotated text); clipping is ignored.
* **Annotations.** Highlight colours are not read (Word's yellow is used);
  free-text annotations are kept as pictures, not editable text. Footnotes
  are not detected in exact mode (frames cannot hold them).
* **OCR** classifies fonts as serif, sans or monospace only, not by name.
  Very short words inherit bold or italic from their line. Automatic language
  detection chooses among the installed languages only.
* **Right-to-left** lists are kept as plain text, without hanging indents.
* **PDF text editing** works on whole lines (runs of the same font), not on
  paragraphs: text does not re-wrap to the next line. Hebrew, Arabic and other
  right-to-left text cannot be edited yet (it would need reordering and
  shaping), nor text inside form XObjects (some PDFs draw whole pages that
  way) or vertical text. Replacement letters missing from an embedded font
  subset come from an installed font, so they can look different. Saving
  rewrites the file, which invalidates digital signatures.
