//! Geometry primitives and the intermediate document model shared by the
//! extractor, the OCR engine, the layout analyser and the DOCX writer.
//!
//! All coordinates are in PDF points (1/72 inch) with the origin in the
//! **top-left** corner of the displayed page and `y` growing downwards.

use std::fmt;

/// Axis-aligned rectangle (top-left origin).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// Builds a rectangle from two arbitrary corners.
    pub fn from_points(ax: f32, ay: f32, bx: f32, by: f32) -> Self {
        Self::new(ax.min(bx), ay.min(by), ax.max(bx), ay.max(by))
    }

    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }

    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }

    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }

    pub fn cx(&self) -> f32 {
        (self.x0 + self.x1) * 0.5
    }

    pub fn cy(&self) -> f32 {
        (self.y0 + self.y1) * 0.5
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(
            self.x0.min(o.x0),
            self.y0.min(o.y0),
            self.x1.max(o.x1),
            self.y1.max(o.y1),
        )
    }

    pub fn intersection(&self, o: &Rect) -> Rect {
        Rect::new(
            self.x0.max(o.x0),
            self.y0.max(o.y0),
            self.x1.min(o.x1),
            self.y1.min(o.y1),
        )
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    /// Length of the overlap of the two horizontal extents (may be negative = gap).
    pub fn h_overlap(&self, o: &Rect) -> f32 {
        self.x1.min(o.x1) - self.x0.max(o.x0)
    }

    /// Length of the overlap of the two vertical extents (may be negative = gap).
    pub fn v_overlap(&self, o: &Rect) -> f32 {
        self.y1.min(o.y1) - self.y0.max(o.y0)
    }

    /// Fraction of `self`'s area that lies inside `o`.
    pub fn fraction_inside(&self, o: &Rect) -> f32 {
        let a = self.area();
        if a <= 0.0 {
            return if o.contains_point(self.cx(), self.cy()) { 1.0 } else { 0.0 };
        }
        self.intersection(o).area().max(0.0) / a
    }

    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    pub fn contains(&self, o: &Rect) -> bool {
        o.x0 >= self.x0 && o.x1 <= self.x1 && o.y0 >= self.y0 && o.y1 <= self.y1
    }

    pub fn expand(&self, d: f32) -> Rect {
        Rect::new(self.x0 - d, self.y0 - d, self.x1 + d, self.y1 + d)
    }

    pub fn union_all<'a>(rects: impl IntoIterator<Item = &'a Rect>) -> Option<Rect> {
        rects.into_iter().fold(None, |acc, r| match acc {
            None => Some(*r),
            Some(a) => Some(a.union(r)),
        })
    }
}

/// 24-bit RGB colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);
    pub const WHITE: Rgb = Rgb(255, 255, 255);

    pub fn hex(&self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }

    pub fn luminance(&self) -> f32 {
        0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32
    }

    pub fn is_near_white(&self) -> bool {
        self.0 > 245 && self.1 > 245 && self.2 > 245
    }

    /// Snaps almost-black colours (anti-aliasing / CMYK conversion noise) to pure black.
    pub fn snapped(self) -> Rgb {
        if self.0 < 40 && self.1 < 40 && self.2 < 40 {
            Rgb::BLACK
        } else {
            self
        }
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.hex())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VertAlign {
    #[default]
    Baseline,
    Superscript,
    Subscript,
}

/// Character formatting.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Word-compatible font family name (e.g. "Times New Roman").
    pub font: String,
    /// Font size in points.
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub small_caps: bool,
    pub color: Rgb,
    /// Background highlight behind the run.
    pub highlight: Option<Rgb>,
    pub vert: VertAlign,
    /// Baseline shift in points (positive = raised) for super/subscripts.
    pub baseline_shift: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font: "Times New Roman".into(),
            size: 11.0,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            small_caps: false,
            color: Rgb::BLACK,
            highlight: None,
            vert: VertAlign::Baseline,
            baseline_shift: 0.0,
        }
    }
}

impl TextStyle {
    /// Equality used when merging adjacent runs (sizes are compared with a tolerance).
    pub fn same_run_format(&self, o: &TextStyle) -> bool {
        self.font == o.font
            && (self.size - o.size).abs() < 0.26
            && self.bold == o.bold
            && self.italic == o.italic
            && self.underline == o.underline
            && self.strike == o.strike
            && self.small_caps == o.small_caps
            && self.color == o.color
            && self.highlight == o.highlight
            && self.vert == o.vert
            && (self.baseline_shift - o.baseline_shift).abs() < 0.3
    }
}

/// A single positioned character.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub ch: char,
    pub bbox: Rect,
    /// Baseline y (top-left origin).
    pub baseline: f32,
    /// Index into [`RawPage::styles`].
    pub style: usize,
    /// The source text stream had whitespace right before this glyph.
    pub space_before: bool,
    /// Hyperlink target index into [`RawPage::links`], if any.
    pub link: Option<usize>,
}

/// A raster image placed on the page.
#[derive(Clone, Debug)]
pub struct RawImage {
    pub bbox: Rect,
    pub data: ImageData,
    /// The image sits behind text (e.g. a watermark or a background).
    pub behind_text: bool,
}

#[derive(Clone, Debug)]
pub struct ImageData {
    /// Encoded bytes (PNG or JPEG).
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub px_width: u32,
    pub px_height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleOrientation {
    Horizontal,
    Vertical,
}

/// A thin straight line (table border, underline, separator).
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub bbox: Rect,
    pub orientation: RuleOrientation,
    pub color: Rgb,
    /// Stroke thickness in points.
    pub width: f32,
}

/// A filled rectangle (cell shading, highlight box).
#[derive(Clone, Copy, Debug)]
pub struct FilledBox {
    pub bbox: Rect,
    pub color: Rgb,
}

/// Everything extracted from one PDF page before layout analysis.
#[derive(Clone, Debug, Default)]
pub struct RawPage {
    pub index: usize,
    pub width: f32,
    pub height: f32,
    pub glyphs: Vec<Glyph>,
    pub styles: Vec<TextStyle>,
    pub images: Vec<RawImage>,
    pub rules: Vec<Rule>,
    pub boxes: Vec<FilledBox>,
    pub links: Vec<String>,
    /// The text came from OCR rather than the PDF text layer.
    pub ocr: bool,
}

impl RawPage {
    /// Interns a style and returns its index.
    pub fn style_id(&mut self, style: TextStyle) -> usize {
        if let Some(i) = self.styles.iter().position(|s| *s == style) {
            i
        } else {
            self.styles.push(style);
            self.styles.len() - 1
        }
    }
}

// ---------------------------------------------------------------------------
// Laid-out document model (output of the layout analyser, input of the writer)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Inline {
    Text {
        text: String,
        style: TextStyle,
        link: Option<String>,
    },
    Tab,
    LineBreak,
    /// End of a line in the source PDF. Flow layout joins the lines,
    /// exact layout keeps the break.
    SoftBreak(Joiner),
}

/// How two source lines are joined in flow layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Joiner {
    /// Insert a space.
    Space,
    /// Join directly (line ended with a dash or slash).
    Direct,
    /// The line ended with a hyphenation hyphen which was removed; exact layout re-inserts it.
    Hyphen,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, Debug)]
pub struct TabStop {
    /// Position relative to the paragraph's container left edge, in points.
    pub pos: f32,
    pub kind: TabKind,
}

#[derive(Clone, Debug, Default)]
pub struct Paragraph {
    pub inlines: Vec<Inline>,
    pub align: Align,
    /// Left indent relative to the container (points).
    pub indent_left: f32,
    pub indent_right: f32,
    /// First-line indent; negative = hanging indent.
    pub indent_first: f32,
    pub space_before: f32,
    pub space_after: f32,
    /// Exact line pitch in points (None = single spacing).
    pub line_pitch: Option<f32>,
    /// Baseline of the first line on the source page.
    pub first_baseline: f32,
    /// Number of lines on the source page.
    pub n_lines: usize,
    /// Dominant font size of the paragraph.
    pub font_size: f32,
    pub tabs: Vec<TabStop>,
    /// Heading level 1..=3, if detected.
    pub heading: Option<u8>,
    /// Bounding box on the source page.
    pub bbox: Rect,
    /// Background shading.
    pub shading: Option<Rgb>,
}

impl Paragraph {
    pub fn plain_text(&self) -> String {
        let mut s = String::new();
        for i in &self.inlines {
            match i {
                Inline::Text { text, .. } => s.push_str(text),
                Inline::Tab => s.push('\t'),
                Inline::LineBreak => s.push('\n'),
                Inline::SoftBreak(Joiner::Space) => s.push(' '),
                Inline::SoftBreak(_) => {}
            }
        }
        s
    }

    /// Largest font size used in the paragraph.
    pub fn max_font_size(&self) -> f32 {
        self.inlines
            .iter()
            .filter_map(|i| match i {
                Inline::Text { style, .. } => Some(style.size),
                _ => None,
            })
            .fold(0.0, f32::max)
    }
}

#[derive(Clone, Debug)]
pub struct Border {
    pub color: Rgb,
    pub width: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub blocks: Vec<Block>,
    /// Number of grid columns this cell spans.
    pub col_span: usize,
    /// This cell continues a vertical merge started above.
    pub v_merge_continue: bool,
    /// This cell starts a vertical merge.
    pub v_merge_start: bool,
    pub shading: Option<Rgb>,
    pub border_top: Option<Border>,
    pub border_bottom: Option<Border>,
    pub border_left: Option<Border>,
    pub border_right: Option<Border>,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Minimum row height in points.
    pub height: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    /// Width of each grid column in points.
    pub col_widths: Vec<f32>,
    pub rows: Vec<Row>,
    /// Left offset relative to the container (points).
    pub indent: f32,
    pub space_before: f32,
    pub bbox: Rect,
    /// A layout table (multi-column text) rather than a data table.
    pub layout_only: bool,
}

#[derive(Clone, Debug)]
pub struct Picture {
    pub data: ImageData,
    pub width: f32,
    pub height: f32,
    pub bbox: Rect,
    pub indent: f32,
    pub space_before: f32,
    pub align: Align,
    /// Floating picture anchored at its absolute page position: behind the
    /// text (true) or with square text wrapping (false). Only used for
    /// pictures in [`PageLayout::floating`].
    pub behind_text: bool,
}

/// A horizontal separator line.
#[derive(Clone, Debug)]
pub struct RuleBlock {
    pub bbox: Rect,
    pub color: Rgb,
    pub width: f32,
    pub indent_left: f32,
    pub indent_right: f32,
    pub space_before: f32,
}

#[derive(Clone, Debug)]
pub enum Block {
    Paragraph(Paragraph),
    Table(Table),
    Picture(Picture),
    Rule(RuleBlock),
}

impl Block {
    pub fn bbox(&self) -> Rect {
        match self {
            Block::Paragraph(p) => p.bbox,
            Block::Table(t) => t.bbox,
            Block::Picture(p) => p.bbox,
            Block::Rule(r) => r.bbox,
        }
    }
}

/// A fully analysed page ready for serialisation.
#[derive(Clone, Debug, Default)]
pub struct PageLayout {
    pub index: usize,
    pub width: f32,
    pub height: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub blocks: Vec<Block>,
    /// Pictures anchored at absolute positions (backgrounds, watermarks,
    /// figures with text flowing beside them).
    pub floating: Vec<Picture>,
    pub ocr: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_ops() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 15.0, 20.0);
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 20.0));
        assert!(a.intersects(&b));
        assert_eq!(a.h_overlap(&b), 5.0);
        assert!((a.fraction_inside(&b) - 0.25).abs() < 1e-6);
        assert_eq!(Rect::from_points(3.0, 4.0, 1.0, 2.0), Rect::new(1.0, 2.0, 3.0, 4.0));
    }

    #[test]
    fn colour_snapping() {
        assert_eq!(Rgb(20, 25, 30).snapped(), Rgb::BLACK);
        assert_eq!(Rgb(200, 0, 0).snapped(), Rgb(200, 0, 0));
        assert_eq!(Rgb(255, 0, 16).hex(), "FF0010");
    }
}
