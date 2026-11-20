# pdf2word — PDF to Word converter (Rust · PDFium · Qt · OCR)

Converts PDF documents to editable Word (`.docx`) files while keeping the
layout and formatting of the original, including scanned pages through OCR.

* **Rust** conversion engine (`pdf2word-core`), command-line tool (`pdf2word`)
  and a **Qt 6 / QML** desktop application (`pdf2word-gui`, built with [CXX-Qt](https://kdab.github.io/cxx-qt/)).
* **PDFium** (via [`pdfium-render`](https://crates.io/crates/pdfium-render)) parses and renders PDFs.
* **Tesseract** OCR for scanned pages, linked through its stable C API.
* A purpose-built WordprocessingML writer: no Office, LibreOffice or other runtime needed.

## What is preserved

| Area | Details |
|---|---|
| Characters | font family (PostScript names mapped to Word fonts), size, bold, italic, small caps, colour, underline, strike-through, highlight, super/subscripts with exact baseline shift, hyperlinks, ligatures expanded |
| Fonts | the PDF's own TrueType fonts are **embedded** into the DOCX when their licence allows (subset fonts get a rebuilt Unicode map); otherwise each run gets **character spacing / scaling** so that it keeps its PDF width with the font Word substitutes (built-in metrics of Word's core fonts plus installed system fonts) |
| Paragraphs | left/centre/right/justified alignment, left/right indents, first-line and hanging indents, exact line pitch, original vertical spacing, tab stops (including right tabs for page numbers), hyphenation joined in flowing text, intentional line breaks (addresses, titles) kept |
| Structure | multi-column layouts (recursive XY-cut, reading order preserved), bulleted/numbered lists, heading styles (Heading 1–3 for the navigation pane), horizontal rules |
| Scripts | **right-to-left** Hebrew and Arabic (visual → logical order, mirrored brackets, Arabic presentation forms normalised, bidi paragraphs and tables); **rotated text** and **vertical East Asian text** as positioned text boxes |
| Equations | maths fonts (TeX, Cambria Math, STIX …) become **native Word equations (OMML)**: fractions, radicals, sums/integrals with limits, super/subscripts, inline and display; matrices and case distinctions are kept as pictures |
| Tables | ruled tables with merged cells (row/column spans), per-edge borders, cell shading; whitespace-aligned tables (invoices, statements) with per-column alignment |
| Graphics | embedded raster images (PNG/JPEG), vector charts and diagrams rasterised with their labels, background images/watermarks behind text, pictures with text flowing beside them |
| Pages | page size and orientation per page, margins derived from content, one section per page so pages stay aligned |
| Scans | Tesseract OCR with per-letter boxes and baselines; **bold** (stroke width), **italic** (slant) and **font class** (serif / sans / monospace → Times New Roman / Arial / Courier New) from the letter shapes; colour, bullet repair, **table rulings and underlines recovered from pixels**, pictures cropped from the scan; PDFs that already carry an invisible OCR text layer reuse it |

Two output modes:

* **Flow** (default): real, editable paragraphs, columns become borderless
  tables, and text reflows naturally when edited.
* **Exact**: every text block is a frame (`w:framePr`) at its absolute page
  position, with pictures and tables anchored there too. This gives the highest
  visual fidelity and suits forms and complex layouts.

## Architecture

```
            ┌──────────────┐   glyphs, fonts, colours, links,
 PDF ──────▶│  extract.rs  │── images, rules, filled boxes,     ┐
  PDFium    └──────────────┘   rasterised vector figures        │
                │ scanned page?                                 │  RawPage
                ▼                                               │
            ┌──────────────┐   words → pseudo-glyphs, rulings,  │
            │   ocr.rs     │── figures detected in the bitmap   ┘
  Tesseract └──────────────┘
                                         ┌──────────────┐    PageLayout     ┌────────────┐
                                RawPage ▶│  layout.rs   │──────────────────▶│  docx.rs   │──▶ .docx
                                         └──────────────┘ paragraphs,       └────────────┘
                                 segments → XY-cut tree → tables, lists,    flow / exact
                                 lines → paragraphs/tables  pictures, rules
```

* `crates/core`: the library (`convert()`, `inspect()`, `render_page_png()`, progress events, cancellation).
  Modules: `extract` (PDFium), `ocr` (Tesseract), `layout` (reconstruction, bidi, text boxes),
  `math` (equations → OMML), `metrics` (font widths, compensation), `embed` (font embedding), `docx` (writer)
* `crates/cli`: `pdf2word` command-line front-end
* `crates/gui`: Qt Quick application (`src/converter.rs` QObject bridge + `qml/Main.qml`)
* `tools/`: `fetch-pdfium.sh` (prebuilt PDFium download), `make_test_pdfs.py` and `math-fixture.tex`
  (sample documents), `gen_metrics.py` (built-in font metrics), `make_icons.py` (application icon)
* `packaging/`: icons, Windows resource and Inno Setup script, macOS `Info.plist`, Linux `.desktop`

## Downloads

Every push builds ready-to-run packages in GitHub Actions (workflow
**Release builds**); tagged versions (`v*`) publish them as a GitHub release:

| Platform | Package |
|---|---|
| Windows x64 | `pdf2word-setup-<version>-x64.exe` installer (Start menu, optional desktop icon, *Open with* for PDFs) or the portable folder |
| macOS (Apple Silicon) | `pdf2word-<version>-macos-arm64.dmg` (ad-hoc signed: right-click → Open the first time) |
| Linux x86_64 | `pdf2word-<version>-linux-x86_64.AppImage` |

All packages include PDFium, Tesseract and English OCR data. The programs
are not code-signed, so Windows SmartScreen and macOS Gatekeeper warn on first start.

## Building

Requirements:

* Rust 1.85+
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
3. the executable's directory, `./lib`, `../lib`
4. the current directory
5. the system library path

## Usage

### Desktop app

```sh
PDFIUM_DYNAMIC_LIB_PATH=./pdfium target/release/pdf2word-gui [file.pdf]
```

Open or drag-and-drop a PDF, page through the preview, choose the layout mode
and the OCR, content and page options, then click **Convert to Word**. The
conversion runs in the background and can be cancelled. When it finishes, use
**Open document** or **Show folder**.

### Command line

```sh
pdf2word report.pdf                         # → report.docx
pdf2word report.pdf -o out.docx -m exact    # positioned frames
pdf2word scan.pdf --ocr always -l eng+deu   # force OCR, several languages
pdf2word a.pdf b.pdf c.pdf -o converted/    # batch
pdf2word big.pdf -p 1-5,10-                 # page selection
```

Run `pdf2word --help` for all options: `--no-images`, `--no-figures`,
`--no-tables`, `--no-headings`, `--no-embed-fonts`, `--no-width-compensation`,
`--ocr-dpi`, `--ocr-font` (`auto` by default), `--tessdata`, `--password`, `--pdfium`.

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
ToUnicode parsing, font rebuilding and obfuscation, bidi reordering, list and
hyphenation handling, paragraph and column detection, the equation parser and
the DOCX writer. Integration tests (`crates/core/tests/convert.rs`) convert the
sample PDFs in `crates/core/tests/fixtures`. Regenerate them with
`python3 tools/make_test_pdfs.py DIR`, which needs `reportlab`, `fonttools`,
`python-bidi` and `arabic-reshaper`. `math.pdf` comes from
`pdflatex tools/math-fixture.tex`. The integration tests check:

* headings, merged cells, formatting, links, lists, reading order, alignment;
* exact-mode frames and page selection;
* width compensation for condensed and letter-spaced text;
* the embedded font: it must parse, carry the right family name and map every character;
* right-to-left order with numbers and Latin runs;
* rotated and vertical text boxes;
* equations (fractions, radicals, n-ary operators, scripts) and the matrix pictures;
* OCR: table rulings from pixels, and bold/italic/font class on a scanned typography sample.

Without PDFium (or Tesseract language data), those tests are skipped.

`cargo run -p pdf2word-core --example dump_layout -- file.pdf [page]` prints
the analysed layout tree (`DUMP_GLYPHS=1` adds glyphs, rules and rotated text).

## Limitations

* **Fonts.** Embedding needs a TrueType font in the PDF with a usable encoding.
  Type 1 and CFF fonts (common in LaTeX and print PDFs) cannot be embedded in
  DOCX, so width compensation is used for them instead. Embedded subsets only
  contain the characters of the document, so text typed later in Word falls
  back to another font. LibreOffice applies kerning by default, so line breaks
  can differ slightly there. Word does not kern unless told to.
* **Rotated and vertical text** use standard DrawingML text boxes.
  LibreOffice ignores shape rotation (30° text shows horizontally) and turns
  vertical CJK glyphs sideways. This has not been checked in Word.
* **Equations** are rebuilt from geometry. Matrices, case distinctions and
  other two-dimensional layouts are kept as pictures, not editable maths.
  Multi-line aligned equations become one equation per line.
* **OCR** classifies fonts as serif, sans or monospace only, not by name.
  Very short words inherit bold or italic from their line.
* **Right-to-left** lists are kept as plain text, without hanging indents.
* **Installers** are not code-signed or notarised. The macOS build targets
  Apple Silicon only.
