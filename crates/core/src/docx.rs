//! Minimal, dependency-light WordprocessingML (DOCX) writer tailored to the
//! layout model.
//!
//! Two output strategies are supported:
//! * **Flow** – regular, fully editable paragraphs with measured indents,
//!   spacing, tab stops, tables and inline pictures; multi-column regions
//!   become borderless tables.
//! * **Exact** – every text block is a positioned frame (`w:framePr`) and every
//!   picture/table is anchored at its absolute page position, reproducing the
//!   source page as closely as possible.
//! * **Continuous** – flowing text without the PDF's page breaks (see
//!   [`crate::reflow`]): Word columns, joined paragraphs and tables.

use std::fmt::Write as _;
use std::io::{Seek, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::layout::DESCENT;
use crate::model::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LayoutMode {
    /// Editable flowing text (default).
    #[default]
    Flow,
    /// Absolutely positioned frames that mirror the PDF page.
    Exact,
    /// One continuous document: page breaks are not kept, text columns
    /// become Word columns and paragraphs/tables split by a page are joined.
    Continuous,
}

#[derive(Clone, Debug, Default)]
pub struct DocMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    /// Fonts to embed into the package.
    pub fonts: Vec<crate::embed::EmbeddedFont>,
    /// Headers, footers, lists, footnotes and styles.
    pub structure: DocStructure,
    /// Number of the first converted page (for page-number fields).
    pub first_page_number: usize,
}

fn is_rtl_char(c: char) -> bool {
    matches!(unicode_bidi::bidi_class(c), unicode_bidi::BidiClass::R | unicode_bidi::BidiClass::AL)
}

fn tw(pt: f32) -> i64 {
    (pt * 20.0).round() as i64
}

fn emu(pt: f32) -> i64 {
    (pt * 12700.0).round() as i64
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            // Characters not allowed in XML 1.0.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => o.push(c),
        }
    }
    o
}

struct Media {
    name: String,
    bytes: Vec<u8>,
}

/// Page setup of one section.
struct SectGeom<'a> {
    width: f32,
    height: f32,
    /// top, right, bottom, left
    margins: [f32; 4],
    columns: &'a [crate::reflow::ColumnDef],
    /// Starts on the same page as the previous section.
    continuous: bool,
    /// Free space below the body content on the source page.
    free_bottom: Option<f32>,
    /// Source page (page-by-page layouts).
    page: Option<usize>,
}

/// (rId, type, target, external)
type Rel = (String, &'static str, String, bool);

struct Writer {
    mode: LayoutMode,
    body: String,
    media: Vec<Media>,
    rels: Vec<Rel>,
    next_rel: usize,
    next_drawing: usize,
    fonts: std::collections::BTreeMap<String, Generic>,
    /// Paragraph styles by id.
    styles: std::collections::HashMap<String, StyleDef>,
    /// Style of the paragraph being written: runs only carry what differs.
    base: Option<StyleDef>,
    /// Header/footer references and page setup shared by every section.
    sect_refs: String,
    header_distance: f32,
    footer_distance: f32,
    /// Room the header/footer needs (distance + content height).
    header_room: f32,
    footer_room: f32,
    blank_first: (bool, bool),
    /// Pages without header / footer, and the blank parts their sections use.
    no_header: std::collections::HashSet<usize>,
    no_footer: std::collections::HashSet<usize>,
    blank_refs: String,
    page_numbers: std::collections::HashMap<usize, (i64, bool)>,
    title_page: bool,
    first_page_number: usize,
    sections: usize,
    control_id: usize,
    bookmark_id: usize,
}

const REL_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const REL_LINK: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

impl Writer {
    fn new(mode: LayoutMode) -> Self {
        Self {
            mode,
            body: String::with_capacity(1 << 20),
            media: Vec::new(),
            rels: Vec::new(),
            next_rel: 10,
            next_drawing: 1,
            fonts: Default::default(),
            styles: Default::default(),
            base: None,
            sect_refs: String::new(),
            header_distance: 0.0,
            footer_distance: 0.0,
            header_room: 0.0,
            footer_room: 0.0,
            blank_first: (false, false),
            no_header: Default::default(),
            no_footer: Default::default(),
            blank_refs: String::new(),
            page_numbers: Default::default(),
            title_page: false,
            first_page_number: 1,
            sections: 0,
            control_id: 0,
            bookmark_id: 0,
        }
    }

    /// Writes blocks into a separate part (header, footer, footnote) with its
    /// own relationships; returns the XML and those relationships.
    fn part(&mut self, f: impl FnOnce(&mut Self, &mut String)) -> (String, Vec<Rel>) {
        let saved = std::mem::take(&mut self.rels);
        let mut out = String::new();
        f(self, &mut out);
        let rels = std::mem::replace(&mut self.rels, saved);
        (out, rels)
    }

    /// Style a paragraph is written with (its own, its heading, or Normal).
    fn style_of(&self, p: &Paragraph) -> Option<StyleDef> {
        let id = p.style.clone().or_else(|| p.heading.map(|h| format!("Heading{h}")));
        id.as_deref().and_then(|id| self.styles.get(id)).or_else(|| self.styles.get("Normal")).cloned()
    }

    fn add_rel(&mut self, ty: &'static str, target: String, external: bool) -> String {
        if external {
            if let Some((id, ..)) = self.rels.iter().find(|(_, t, tg, _)| *t == ty && *tg == target) {
                return id.clone();
            }
        }
        let id = format!("rId{}", self.next_rel);
        self.next_rel += 1;
        self.rels.push((id.clone(), ty, target, external));
        id
    }

    fn add_image(&mut self, data: &ImageData) -> String {
        let name = format!("image{}.{}", self.media.len() + 1, data.ext);
        self.media.push(Media { name: name.clone(), bytes: data.bytes.clone() });
        self.add_rel(REL_IMAGE, format!("media/{name}"), false)
    }

    // ------------------------------------------------------------------ runs

    fn run_props(&mut self, st: &TextStyle) -> String {
        self.fonts.entry(st.font.clone()).or_insert(st.generic);
        let mut r = String::from("<w:rPr>");
        let base = self.base.take();
        let hp = (st.size * 2.0).round().max(2.0) as i64;
        let (same_font, base_bold, base_italic, base_color, same_size) = match &base {
            Some(b) => (b.font == st.font, b.bold, b.italic, b.color, (b.size * 2.0).round() as i64 == hp),
            None => (false, false, false, Rgb::BLACK, false),
        };
        self.base = base;
        if !same_font {
            let f = esc(&st.font);
            let _ = write!(r, r#"<w:rFonts w:ascii="{f}" w:hAnsi="{f}" w:cs="{f}" w:eastAsia="{f}"/>"#);
        }
        if st.bold != base_bold {
            r.push_str(if st.bold { "<w:b/><w:bCs/>" } else { r#"<w:b w:val="0"/><w:bCs w:val="0"/>"# });
        }
        if st.italic != base_italic {
            r.push_str(if st.italic { "<w:i/><w:iCs/>" } else { r#"<w:i w:val="0"/><w:iCs w:val="0"/>"# });
        }
        if st.small_caps {
            r.push_str("<w:smallCaps/>");
        }
        if st.strike {
            r.push_str("<w:strike/>");
        }
        if st.color != base_color {
            let _ = write!(r, r#"<w:color w:val="{}"/>"#, st.color.hex());
        }
        if st.char_spacing.abs() >= 0.05 {
            let _ = write!(r, r#"<w:spacing w:val="{}"/>"#, (st.char_spacing * 20.0).round() as i64);
        }
        if st.char_scale != 100 {
            let _ = write!(r, r#"<w:w w:val="{}"/>"#, st.char_scale.clamp(1, 600));
        }
        if st.vert != VertAlign::Baseline && st.baseline_shift.abs() >= 0.5 {
            let _ = write!(r, r#"<w:position w:val="{}"/>"#, (st.baseline_shift * 2.0).round() as i64);
        }
        if !same_size {
            let _ = write!(r, r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#);
        }
        if st.underline {
            r.push_str(r#"<w:u w:val="single"/>"#);
        }
        if let Some(h) = st.highlight {
            let _ = write!(r, r#"<w:shd w:val="clear" w:color="auto" w:fill="{}"/>"#, h.hex());
        }
        // Scripts with a measured shift use w:position (exact size); otherwise vertAlign.
        if st.baseline_shift.abs() < 0.5 {
            match st.vert {
                VertAlign::Superscript => r.push_str(r#"<w:vertAlign w:val="superscript"/>"#),
                VertAlign::Subscript => r.push_str(r#"<w:vertAlign w:val="subscript"/>"#),
                VertAlign::Baseline => {}
            }
        }
        r.push_str("</w:rPr>");
        r
    }

    /// Serialises an equation as Office Math (OMML).
    fn omml(&mut self, nodes: &[MathNode], _inline: bool) -> String {
        self.fonts.entry("Cambria Math".into()).or_insert(Generic::Serif);
        let mut s = String::from("<m:oMath>");
        Self::math_nodes(&mut s, nodes);
        s.push_str("</m:oMath>");
        s
    }

    fn math_nodes(s: &mut String, nodes: &[MathNode]) {
        for n in nodes {
            match n {
                MathNode::Run { text, upright, bold, size } => {
                    s.push_str("<m:r>");
                    let sty = match (*upright, *bold) {
                        (true, false) => Some("p"),
                        (true, true) => Some("b"),
                        (false, true) => Some("bi"),
                        (false, false) => None,
                    };
                    if let Some(sty) = sty {
                        let _ = write!(s, r#"<m:rPr><m:sty m:val="{sty}"/></m:rPr>"#);
                    }
                    let hp = (size * 2.0).round().max(2.0) as i64;
                    let _ = write!(
                        s,
                        r#"<w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/><w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/></w:rPr><m:t xml:space="preserve">{}</m:t></m:r>"#,
                        esc(text)
                    );
                }
                MathNode::Frac { num, den } => {
                    s.push_str("<m:f><m:num>");
                    Self::math_nodes(s, num);
                    s.push_str("</m:num><m:den>");
                    Self::math_nodes(s, den);
                    s.push_str("</m:den></m:f>");
                }
                MathNode::Script { base, sub, sup } => {
                    let tag = match (sub.is_some(), sup.is_some()) {
                        (true, true) => "sSubSup",
                        (true, false) => "sSub",
                        (false, true) => "sSup",
                        (false, false) => {
                            Self::math_nodes(s, base);
                            continue;
                        }
                    };
                    let _ = write!(s, "<m:{tag}><m:e>");
                    Self::math_nodes(s, base);
                    s.push_str("</m:e>");
                    if let Some(b) = sub {
                        s.push_str("<m:sub>");
                        Self::math_nodes(s, b);
                        s.push_str("</m:sub>");
                    }
                    if let Some(p) = sup {
                        s.push_str("<m:sup>");
                        Self::math_nodes(s, p);
                        s.push_str("</m:sup>");
                    }
                    let _ = write!(s, "</m:{tag}>");
                }
                MathNode::Radical { degree, body } => {
                    s.push_str("<m:rad>");
                    match degree {
                        Some(d) => {
                            s.push_str("<m:deg>");
                            Self::math_nodes(s, d);
                            s.push_str("</m:deg>");
                        }
                        None => s.push_str(r#"<m:radPr><m:degHide m:val="1"/></m:radPr><m:deg/>"#),
                    }
                    s.push_str("<m:e>");
                    Self::math_nodes(s, body);
                    s.push_str("</m:e></m:rad>");
                }
                MathNode::Nary { op, sub, sup, body } => {
                    let _ = write!(s, r#"<m:nary><m:naryPr><m:chr m:val="{}"/><m:limLoc m:val="undOvr"/>"#, esc(&op.to_string()));
                    if sub.is_none() {
                        s.push_str(r#"<m:subHide m:val="1"/>"#);
                    }
                    if sup.is_none() {
                        s.push_str(r#"<m:supHide m:val="1"/>"#);
                    }
                    s.push_str("</m:naryPr><m:sub>");
                    if let Some(b) = sub {
                        Self::math_nodes(s, b);
                    }
                    s.push_str("</m:sub><m:sup>");
                    if let Some(p) = sup {
                        Self::math_nodes(s, p);
                    }
                    s.push_str("</m:sup><m:e>");
                    Self::math_nodes(s, body);
                    s.push_str("</m:e></m:nary>");
                }
            }
        }
    }

    fn text_run(&mut self, out: &mut String, text: &str, st: &TextStyle) {
        let mut rpr = self.run_props(st);
        if text.chars().any(is_rtl_char) {
            rpr = rpr.replace("</w:rPr>", "<w:rtl/></w:rPr>");
        }
        let _ = write!(out, r#"<w:r>{rpr}<w:t xml:space="preserve">{}</w:t></w:r>"#, esc(text));
    }

    /// Tabs and line ends (also inside hyperlinks).
    fn break_inline(&mut self, out: &mut String, inl: &Inline, last_style: &Option<TextStyle>) {
        match inl {
            Inline::Tab => {
                let rpr = last_style.clone().map(|s| self.run_props(&s)).unwrap_or_default();
                let _ = write!(out, "<w:r>{rpr}<w:tab/></w:r>");
            }
            Inline::LineBreak => out.push_str("<w:r><w:br/></w:r>"),
            Inline::SoftBreak(j) => {
                let st = last_style.clone().unwrap_or_default();
                if self.mode == LayoutMode::Exact {
                    if *j == Joiner::Hyphen {
                        self.text_run(out, "-", &st);
                    }
                    let rpr = self.run_props(&st);
                    let _ = write!(out, "<w:r>{rpr}<w:br/></w:r>");
                } else if *j == Joiner::Space {
                    self.text_run(out, " ", &st);
                }
            }
            _ => {}
        }
    }

    /// A form field as a content control.
    fn control(&mut self, out: &mut String, c: &FormControl, style: &TextStyle) {
        self.control_id += 1;
        let id = self.control_id;
        let name = esc(&c.name);
        let rpr = self.run_props(style);
        let _ = write!(out, r#"<w:sdt><w:sdtPr><w:alias w:val="{name}"/><w:tag w:val="{name}"/><w:id w:val="{id}"/>"#);
        match &c.kind {
            ControlKind::Text { value, multiline } => {
                let empty = value.trim().is_empty();
                if empty {
                    out.push_str("<w:showingPlcHdr/>");
                }
                let _ = write!(out, r#"<w:text{}/></w:sdtPr><w:sdtContent>"#, if *multiline { r#" w:multiLine="1""# } else { "" });
                // A light box shows where to type; empty fields keep their width.
                let rpr = rpr.replace("</w:rPr>", r#"<w:bdr w:val="single" w:sz="4" w:space="0" w:color="A0A0A0"/></w:rPr>"#);
                let text = if empty {
                    let n = (c.width / (style.size.max(4.0) * 0.5)).clamp(2.0, 80.0) as usize;
                    "\u{2002}".repeat(n)
                } else {
                    value.clone()
                };
                let _ = write!(out, r#"<w:r>{rpr}<w:t xml:space="preserve">{}</w:t></w:r>"#, esc(&text));
            }
            ControlKind::Check { checked } => {
                let _ = write!(
                    out,
                    r#"<w14:checkbox><w14:checked w14:val="{}"/><w14:checkedState w14:val="2612" w14:font="MS Gothic"/><w14:uncheckedState w14:val="2610" w14:font="MS Gothic"/></w14:checkbox></w:sdtPr><w:sdtContent>"#,
                    *checked as u8
                );
                self.fonts.entry("MS Gothic".into()).or_insert(Generic::Sans);
                let rpr = rpr.replace("<w:rPr>", r#"<w:rPr><w:rFonts w:ascii="MS Gothic" w:eastAsia="MS Gothic" w:hAnsi="MS Gothic" w:hint="eastAsia"/>"#);
                // run_props may have written the text font first: the later rFonts would be ignored.
                let rpr = match rpr.find("<w:rFonts w:ascii=\"MS Gothic\"") {
                    Some(_) => {
                        let mut r = rpr.clone();
                        if let (Some(a), Some(b)) = (r.rfind("<w:rFonts w:ascii="), r.rfind("<w:rFonts w:ascii=\"MS Gothic\"")) {
                            if a != b {
                                let end = r[a..].find("/>").map(|e| a + e + 2).unwrap_or(a);
                                r.replace_range(a..end, "");
                            }
                        }
                        r
                    }
                    None => rpr,
                };
                let _ = write!(out, r#"<w:r>{rpr}<w:t>{}</w:t></w:r>"#, if *checked { '☒' } else { '☐' });
            }
            ControlKind::Dropdown { items, value } => {
                out.push_str("<w:dropDownList>");
                for it in items {
                    let e = esc(it);
                    let _ = write!(out, r#"<w:listItem w:displayText="{e}" w:value="{e}"/>"#);
                }
                out.push_str("</w:dropDownList></w:sdtPr><w:sdtContent>");
                let rpr = rpr.replace("</w:rPr>", r#"<w:bdr w:val="single" w:sz="4" w:space="0" w:color="A0A0A0"/></w:rPr>"#);
                let shown = if value.is_empty() { items.first().cloned().unwrap_or_default() } else { value.clone() };
                let _ = write!(out, r#"<w:r>{rpr}<w:t xml:space="preserve">{}</w:t></w:r>"#, esc(&shown));
            }
        }
        out.push_str("</w:sdtContent></w:sdt>");
    }

    fn inlines(&mut self, out: &mut String, inlines: &[Inline]) {
        let mut last_style: Option<TextStyle> = None;
        let mut i = 0;
        while i < inlines.len() {
            match &inlines[i] {
                Inline::Math(m) => {
                    let x = self.omml(m, true);
                    out.push_str(&x);
                }
                Inline::Text { text, style, link, .. } => {
                    if let Some(url) = link {
                        // Internal targets ("#name") are bookmarks; others are external.
                        if let Some(anchor) = url.strip_prefix('#') {
                            let _ = write!(out, r#"<w:hyperlink w:anchor="{}" w:history="1">"#, esc(anchor));
                        } else {
                            let id = self.add_rel(REL_LINK, url.clone(), true);
                            let _ = write!(out, r#"<w:hyperlink r:id="{id}" w:history="1">"#);
                        }
                        // Group consecutive runs with the same link (tabs and line ends included).
                        let mut j = i;
                        while j < inlines.len() {
                            match &inlines[j] {
                                Inline::Text { text, style, link: Some(l), .. } if l == url => {
                                    self.text_run(out, text, style);
                                    last_style = Some(style.clone());
                                    j += 1;
                                }
                                inl @ (Inline::Tab | Inline::SoftBreak(_) | Inline::LineBreak)
                                    if inlines[j + 1..].iter().find(|x| !matches!(x, Inline::Tab | Inline::SoftBreak(_) | Inline::LineBreak)).is_some_and(|x| matches!(x, Inline::Text { link: Some(l), .. } if l == url)) =>
                                {
                                    self.break_inline(out, inl, &last_style);
                                    j += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push_str("</w:hyperlink>");
                        i = j;
                        continue;
                    }
                    self.text_run(out, text, style);
                    last_style = Some(style.clone());
                }
                inl @ (Inline::Tab | Inline::LineBreak | Inline::SoftBreak(_)) => self.break_inline(out, inl, &last_style),
                Inline::Field { kind, text, style } => {
                    let instr = match kind {
                        FieldKind::PageNumber => " PAGE ",
                        FieldKind::PageCount => " NUMPAGES ",
                    };
                    let rpr = self.run_props(style);
                    let _ = write!(out, r#"<w:fldSimple w:instr="{instr}"><w:r>{rpr}<w:t xml:space="preserve">{}</w:t></w:r></w:fldSimple>"#, esc(text));
                    last_style = Some(style.clone());
                }
                Inline::FootnoteRef(id) => {
                    let _ = write!(out, r#"<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteReference w:id="{}"/></w:r>"#, id + 1);
                }
                Inline::Control { control, style } => {
                    self.control(out, control, style);
                    last_style = Some(style.clone());
                }
                Inline::FieldBegin(instr) => {
                    let _ = write!(
                        out,
                        r#"<w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">{}</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r>"#,
                        esc(instr)
                    );
                }
                Inline::FieldEnd => out.push_str(r#"<w:r><w:fldChar w:fldCharType="end"/></w:r>"#),
            }
            i += 1;
        }
    }

    // ------------------------------------------------------------ paragraphs

    #[allow(clippy::too_many_arguments)]
    fn paragraph(&mut self, out: &mut String, p: &Paragraph, frame: Option<(f32, f32, f32, f32)>, sect: Option<&str>) {
        out.push_str("<w:p><w:pPr>");
        if let Some(id) = p.style.as_deref().filter(|id| self.styles.contains_key(*id)) {
            let _ = write!(out, r#"<w:pStyle w:val="{}"/>"#, esc(id));
        } else if let Some(h) = p.heading {
            let _ = write!(out, r#"<w:pStyle w:val="Heading{h}"/>"#);
        }
        if let Some((x, y, w, h)) = frame {
            let _ = write!(
                out,
                r#"<w:framePr w:w="{}" w:h="{}" w:hRule="atLeast" w:wrap="around" w:hAnchor="page" w:vAnchor="page" w:x="{}" w:y="{}"/>"#,
                tw(w),
                tw(h),
                tw(x),
                tw(y)
            );
        }
        if let Some(l) = p.list {
            let _ = write!(out, r#"<w:numPr><w:ilvl w:val="{}"/><w:numId w:val="{}"/></w:numPr>"#, l.level, l.list + 1);
        }
        if let Some(shd) = p.shading {
            let _ = write!(out, r#"<w:shd w:val="clear" w:color="auto" w:fill="{}"/>"#, shd.hex());
        }
        if !p.tabs.is_empty() {
            out.push_str("<w:tabs>");
            for t in &p.tabs {
                let kind = match t.kind {
                    TabKind::Left => "left",
                    TabKind::Right => "right",
                    TabKind::Center => "center",
                };
                let leader = if t.leader { r#" w:leader="dot""# } else { "" };
                let _ = write!(out, r#"<w:tab w:val="{kind}"{leader} w:pos="{}"/>"#, tw(t.pos.max(0.0)));
            }
            out.push_str("</w:tabs>");
        }
        if p.rtl {
            out.push_str("<w:bidi/>");
        }
        let before = if frame.is_some() { 0.0 } else { p.space_before };
        let _ = write!(out, r#"<w:spacing w:before="{}" w:after="{}""#, tw(before), tw(p.space_after));
        match p.line_pitch {
            Some(l) => {
                let _ = write!(out, r#" w:line="{}" w:lineRule="exact"/>"#, tw(l.max(1.0)));
            }
            None => out.push_str(r#" w:line="240" w:lineRule="auto"/>"#),
        }
        if frame.is_some() {
            if p.indent_first.abs() > 0.5 {
                let (first, hang) = if p.indent_first >= 0.0 { (p.indent_first, 0.0) } else { (0.0, -p.indent_first) };
                if hang > 0.0 {
                    let _ = write!(out, r#"<w:ind w:left="{}" w:hanging="{}"/>"#, tw(hang), tw(hang));
                } else {
                    let _ = write!(out, r#"<w:ind w:left="0" w:firstLine="{}"/>"#, tw(first));
                }
            }
        } else if p.rtl {
            // Right-to-left: indents are logical (start = right edge).
            let _ = write!(out, r#"<w:ind w:start="{}" w:end="{}""#, tw(p.indent_left), tw(p.indent_right));
            if p.indent_first >= 0.0 {
                let _ = write!(out, r#" w:firstLine="{}"/>"#, tw(p.indent_first));
            } else {
                let _ = write!(out, r#" w:hanging="{}"/>"#, tw(-p.indent_first));
            }
        } else {
            let _ = write!(out, r#"<w:ind w:left="{}" w:right="{}""#, tw(p.indent_left), tw(p.indent_right));
            if p.indent_first >= 0.0 {
                let _ = write!(out, r#" w:firstLine="{}"/>"#, tw(p.indent_first));
            } else {
                let _ = write!(out, r#" w:hanging="{}"/>"#, tw(-p.indent_first));
            }
        }
        let jc = match (p.align, p.rtl) {
            (Align::Left, false) => "left",
            (Align::Right, false) => "right",
            // Right-to-left paragraphs were analysed mirrored: left = start.
            (Align::Left, true) => "start",
            (Align::Right, true) => "end",
            (Align::Center, _) => "center",
            (Align::Justify, _) => "both",
        };
        let _ = write!(out, r#"<w:jc w:val="{jc}"/>"#);
        if let Some(s) = sect {
            out.push_str(s);
        }
        out.push_str("</w:pPr>");
        let first_bookmark = self.bookmark_id;
        for name in &p.bookmarks {
            let _ = write!(out, r#"<w:bookmarkStart w:id="{}" w:name="{}"/>"#, self.bookmark_id, esc(name));
            self.bookmark_id += 1;
        }
        for c in &p.comments {
            let _ = write!(out, r#"<w:commentRangeStart w:id="{c}"/>"#);
        }
        let style = self.style_of(p);
        let saved = std::mem::replace(&mut self.base, style);
        self.inlines(out, &p.inlines);
        self.base = saved;
        for c in &p.comments {
            let _ = write!(out, r#"<w:commentRangeEnd w:id="{c}"/><w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="{c}"/></w:r>"#);
        }
        for id in first_bookmark..self.bookmark_id {
            let _ = write!(out, r#"<w:bookmarkEnd w:id="{id}"/>"#);
        }
        out.push_str("</w:p>");
    }

    /// Empty paragraph of a given exact height (vertical spacer / section carrier).
    fn spacer(&mut self, out: &mut String, height: f32, sect: Option<&str>) {
        let h = height.max(0.7);
        let _ = write!(
            out,
            r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="{}" w:lineRule="exact"/>{}<w:rPr><w:sz w:val="2"/><w:szCs w:val="2"/></w:rPr></w:pPr></w:p>"#,
            tw(h),
            sect.unwrap_or("")
        );
    }

    fn rule(&mut self, out: &mut String, r: &RuleBlock) {
        let sz = ((r.width * 8.0).round() as i64).clamp(2, 96);
        let _ = write!(
            out,
            r#"<w:p><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="{sz}" w:space="0" w:color="{}"/></w:pBdr><w:spacing w:before="{}" w:after="0" w:line="20" w:lineRule="exact"/><w:ind w:left="{}" w:right="{}"/><w:rPr><w:sz w:val="2"/><w:szCs w:val="2"/></w:rPr></w:pPr></w:p>"#,
            r.color.hex(),
            tw(r.space_before),
            tw(r.indent_left),
            tw(r.indent_right)
        );
    }

    // -------------------------------------------------------------- pictures

    fn graphic_xml(&mut self, pic: &Picture, id: usize) -> String {
        let rid = self.add_image(&pic.data);
        let (cx, cy) = (emu(pic.width.max(1.0)), emu(pic.height.max(1.0)));
        format!(
            r#"<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:nvPicPr><pic:cNvPr id="{id}" name="Picture {id}"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic>"#
        )
    }

    /// A vector figure as a group of shapes and text boxes (DrawingML).
    fn group_graphic(&mut self, v: &VectorDrawing, w: f32, h: f32) -> String {
        let (cx, cy) = (emu(w.max(1.0)), emu(h.max(1.0)));
        let (sx, sy) = (w / v.width.max(0.01), h / v.height.max(0.01));
        let mut s = format!(
            r#"<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup"><wpg:wgp><wpg:cNvGrpSpPr/><wpg:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/><a:chOff x="0" y="0"/><a:chExt cx="{cx}" cy="{cy}"/></a:xfrm></wpg:grpSpPr>"#
        );
        for shape in &v.shapes {
            let id = self.next_drawing;
            self.next_drawing += 1;
            let b = shape.bbox;
            let (ew, eh) = (emu(b.width() * sx).max(1), emu(b.height() * sy).max(1));
            let pt = |x: f32, y: f32| format!(r#"<a:pt x="{}" y="{}"/>"#, emu((x - b.x0) * sx).clamp(0, ew), emu((y - b.y0) * sy).clamp(0, eh));
            let mut path = String::new();
            for c in &shape.cmds {
                match *c {
                    PathCmd::MoveTo(x, y) => {
                        let _ = write!(path, "<a:moveTo>{}</a:moveTo>", pt(x, y));
                    }
                    PathCmd::LineTo(x, y) => {
                        let _ = write!(path, "<a:lnTo>{}</a:lnTo>", pt(x, y));
                    }
                    PathCmd::CurveTo(x1, y1, x2, y2, x3, y3) => {
                        let _ = write!(path, "<a:cubicBezTo>{}{}{}</a:cubicBezTo>", pt(x1, y1), pt(x2, y2), pt(x3, y3));
                    }
                    PathCmd::Close => path.push_str("<a:close/>"),
                }
            }
            let fill = match shape.fill {
                Some(c) => format!(r#"<a:solidFill><a:srgbClr val="{}"/></a:solidFill>"#, c.hex()),
                None => "<a:noFill/>".into(),
            };
            let line = match shape.stroke {
                Some((c, wd)) => format!(r#"<a:ln w="{}"><a:solidFill><a:srgbClr val="{}"/></a:solidFill></a:ln>"#, emu(wd * sx.min(sy)), c.hex()),
                None => "<a:ln><a:noFill/></a:ln>".into(),
            };
            let path_fill = if shape.fill.is_none() { r#" fill="none""# } else { "" };
            let _ = write!(
                s,
                r#"<wps:wsp><wps:cNvPr id="{id}" name="Shape {id}"/><wps:cNvSpPr/><wps:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{ew}" cy="{eh}"/></a:xfrm><a:custGeom><a:avLst/><a:gdLst/><a:ahLst/><a:cxnLst/><a:rect l="0" t="0" r="r" b="b"/><a:pathLst><a:path w="{ew}" h="{eh}"{path_fill}>{path}</a:path></a:pathLst></a:custGeom>{fill}{line}</wps:spPr><wps:bodyPr/></wps:wsp>"#,
                emu(b.x0 * sx),
                emu(b.y0 * sy)
            );
        }
        let normal = self.styles.get("Normal").cloned();
        let saved = std::mem::replace(&mut self.base, normal);
        for l in &v.labels {
            let id = self.next_drawing;
            self.next_drawing += 1;
            let size = l.style.size.max(1.0);
            let (x0, y0) = (l.bbox.x0 * sx, (l.bbox.y0 - 0.15 * size) * sy);
            let (lw, lh) = (l.bbox.width() * sx + 0.4 * size, (l.bbox.height().max(1.25 * size)) * sy);
            let mut run = String::new();
            self.text_run(&mut run, &l.text, &l.style);
            let _ = write!(
                s,
                r#"<wps:wsp><wps:cNvPr id="{id}" name="Label {id}"/><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="{}" y="{}"/><a:ext cx="{}" cy="{}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></wps:spPr><wps:txbx><w:txbxContent><w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="240" w:lineRule="auto"/></w:pPr>{run}</w:p></w:txbxContent></wps:txbx><wps:bodyPr rot="0" vert="horz" wrap="none" lIns="0" tIns="0" rIns="0" bIns="0" anchor="t" anchorCtr="0"><a:noAutofit/></wps:bodyPr></wps:wsp>"#,
                emu(x0.max(0.0)),
                emu(y0.max(0.0)),
                emu(lw),
                emu(lh)
            );
        }
        self.base = saved;
        s.push_str("</wpg:wgp></a:graphicData></a:graphic>");
        s
    }

    /// Wraps a picture drawing: editable shapes first, the picture as fallback.
    fn with_vector(&mut self, pic: &Picture, drawing: String, frame: impl Fn(&str, usize) -> String) -> String {
        match &pic.vector {
            Some(v) => {
                let id = self.next_drawing;
                self.next_drawing += 1;
                let g = self.group_graphic(v, pic.width.max(1.0), pic.height.max(1.0));
                format!(r#"<w:r><mc:AlternateContent><mc:Choice Requires="wpg">{}</mc:Choice><mc:Fallback>{drawing}</mc:Fallback></mc:AlternateContent></w:r>"#, frame(&g, id))
            }
            None => format!("<w:r>{drawing}</w:r>"),
        }
    }

    fn inline_picture_run(&mut self, pic: &Picture) -> String {
        let id = self.next_drawing;
        self.next_drawing += 1;
        let (cx, cy) = (emu(pic.width.max(1.0)), emu(pic.height.max(1.0)));
        let g = self.graphic_xml(pic, id);
        let frame = move |g: &str, id: usize| {
            format!(
                r#"<w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="{id}" name="Picture {id}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr>{g}</wp:inline></w:drawing>"#
            )
        };
        let drawing = frame(&g, id);
        self.with_vector(pic, drawing, frame)
    }

    fn anchored_picture_run(&mut self, pic: &Picture, behind: bool) -> String {
        self.anchored_picture_run_at(pic, behind, None)
    }

    /// `dy`: vertical offset from the anchor paragraph instead of the page top.
    fn anchored_picture_run_at(&mut self, pic: &Picture, behind: bool, dy: Option<f32>) -> String {
        let id = self.next_drawing;
        self.next_drawing += 1;
        let (cx, cy) = (emu(pic.width.max(1.0)), emu(pic.height.max(1.0)));
        let g = self.graphic_xml(pic, id);
        let wrap = if behind || pic.overlay { "<wp:wrapNone/>".to_string() } else { r#"<wp:wrapSquare wrapText="bothSides"/>"#.to_string() };
        // Overlays (graphic annotations) stack above everything else.
        let rh_base = if pic.overlay { 251700000 } else { 251658240 };
        let bd = if behind { 1 } else { 0 };
        let x = emu(pic.bbox.x0);
        let y = emu(dy.unwrap_or(pic.bbox.y0));
        let vrel = if dy.is_some() { "paragraph" } else { "page" };
        let frame = move |g: &str, id: usize| {
            format!(
                r#"<w:drawing><wp:anchor distT="0" distB="0" distL="57150" distR="57150" simplePos="0" relativeHeight="{rh}" behindDoc="{bd}" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="page"><wp:posOffset>{x}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="{vrel}"><wp:posOffset>{y}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{wrap}<wp:docPr id="{id}" name="Picture {id}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr>{g}</wp:anchor></w:drawing>"#,
                rh = rh_base + id,
            )
        };
        let drawing = frame(&g, id);
        self.with_vector(pic, drawing, frame)
    }

    /// A text box anchored at its page position (rotated or vertical text).
    fn text_box_run(&mut self, tb: &TextBox) -> String {
        self.text_box_run_at(tb, None)
    }

    /// `dy`: offset of the box top from the anchor paragraph instead of the page.
    fn text_box_run_at(&mut self, tb: &TextBox, dy: Option<f32>) -> String {
        let id = self.next_drawing;
        self.next_drawing += 1;
        let (w, h) = (tb.width.max(1.0), tb.height.max(1.0));
        let (cx, cy) = (emu(w), emu(h));
        let mut content = String::from(
            r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="240" w:lineRule="auto"/><w:jc w:val="center"/></w:pPr>"#,
        );
        let normal = self.styles.get("Normal").cloned();
        let saved = std::mem::replace(&mut self.base, normal);
        self.inlines(&mut content, &tb.inlines);
        self.base = saved;
        content.push_str("</w:p>");
        // Quarter turns use the text direction of an upright box (understood by
        // every consumer); other angles rotate the shape itself.
        let r = tb.rotation.rem_euclid(360.0);
        let quarter = |target: f32| (r - target).abs() < 2.0;
        let (vert, rot, w, h, cx, cy) = if tb.vertical {
            ("eaVert", 0, w, h, cx, cy)
        } else if quarter(270.0) {
            ("vert270", 0, h, w, emu(h), emu(w))
        } else if quarter(90.0) {
            ("vert", 0, h, w, emu(h), emu(w))
        } else {
            ("horz", (r * 60000.0).round() as i64, w, h, cx, cy)
        };
        format!(
            r#"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="{rh}" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="page"><wp:posOffset>{x}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="{vrel}"><wp:posOffset>{y}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="{id}" name="Text Box {id}"/><wp:cNvGraphicFramePr/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm rot="{rot}"><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></wps:spPr><wps:txbx><w:txbxContent>{content}</w:txbxContent></wps:txbx><wps:bodyPr rot="0" vert="{vert}" wrap="none" lIns="0" tIns="0" rIns="0" bIns="0" anchor="ctr" anchorCtr="0"><a:noAutofit/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice></mc:AlternateContent></w:r>"#,
            rh = 251660000 + id,
            x = emu(tb.cx - w / 2.0),
            y = emu(dy.map(|d| d + (tb.height.max(tb.width) - h) / 2.0).unwrap_or(tb.cy - h / 2.0)),
            vrel = if dy.is_some() { "paragraph" } else { "page" },
        )
    }

    fn picture_paragraph(&mut self, out: &mut String, pic: &Picture) {
        let run = self.inline_picture_run(pic);
        let jc = if pic.align == Align::Center { "center" } else { "left" };
        let _ = write!(
            out,
            r#"<w:p><w:pPr><w:spacing w:before="{}" w:after="0" w:line="240" w:lineRule="auto"/><w:ind w:left="{}"/><w:jc w:val="{jc}"/></w:pPr>{run}</w:p>"#,
            tw(pic.space_before),
            tw(pic.indent)
        );
    }

    // ---------------------------------------------------------------- tables

    fn border_xml(tag: &str, b: &Option<Border>) -> String {
        match b {
            Some(b) => format!(
                r#"<w:{tag} w:val="single" w:sz="{}" w:space="0" w:color="{}"/>"#,
                ((b.width * 8.0).round() as i64).clamp(2, 96),
                b.color.hex()
            ),
            None => format!(r#"<w:{tag} w:val="nil"/>"#),
        }
    }

    fn table(&mut self, out: &mut String, t: &Table, float_at: Option<(f32, f32)>) {
        if float_at.is_none() && t.space_before > 1.5 {
            self.spacer(out, t.space_before, None);
        }
        let total: f32 = t.col_widths.iter().sum();
        out.push_str("<w:tbl><w:tblPr>");
        if let Some((x, y)) = float_at {
            let _ = write!(
                out,
                r#"<w:tblpPr w:leftFromText="0" w:rightFromText="0" w:vertAnchor="page" w:horzAnchor="page" w:tblpX="{}" w:tblpY="{}"/><w:tblOverlap w:val="overlap"/>"#,
                tw(x),
                tw(y)
            );
        }
        if t.bidi {
            out.push_str("<w:bidiVisual/>");
        }
        let _ = write!(out, r#"<w:tblW w:w="{}" w:type="dxa"/>"#, tw(total));
        if float_at.is_none() {
            let _ = write!(out, r#"<w:tblInd w:w="{}" w:type="dxa"/>"#, tw(t.indent));
        }
        out.push_str(r#"<w:tblBorders><w:top w:val="nil"/><w:left w:val="nil"/><w:bottom w:val="nil"/><w:right w:val="nil"/><w:insideH w:val="nil"/><w:insideV w:val="nil"/></w:tblBorders>"#);
        out.push_str(r#"<w:tblLayout w:type="fixed"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="0" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="0" w:type="dxa"/></w:tblCellMar><w:tblLook w:val="0000" w:firstRow="0" w:lastRow="0" w:firstColumn="0" w:lastColumn="0" w:noHBand="1" w:noVBand="1"/></w:tblPr><w:tblGrid>"#);
        for w in &t.col_widths {
            let _ = write!(out, r#"<w:gridCol w:w="{}"/>"#, tw(*w));
        }
        out.push_str("</w:tblGrid>");
        for (ri, row) in t.rows.iter().enumerate() {
            // Layout tables (columns) may break across pages; data rows stay together.
            out.push_str(if t.layout_only { "<w:tr><w:trPr>" } else { "<w:tr><w:trPr><w:cantSplit/>" });
            if row.height > 0.0 && !t.layout_only {
                let _ = write!(out, r#"<w:trHeight w:val="{}" w:hRule="atLeast"/>"#, tw(row.height));
            }
            if ri < t.header_rows {
                out.push_str("<w:tblHeader/>");
            }
            out.push_str("</w:trPr>");
            let mut col = 0usize;
            for cell in &row.cells {
                let span = cell.col_span.max(1);
                let width: f32 = t.col_widths[col.min(t.col_widths.len() - 1)..(col + span).min(t.col_widths.len())].iter().sum();
                col += span;
                let _ = write!(out, r#"<w:tc><w:tcPr><w:tcW w:w="{}" w:type="dxa"/>"#, tw(width));
                if span > 1 {
                    let _ = write!(out, r#"<w:gridSpan w:val="{span}"/>"#);
                }
                if cell.v_merge_start {
                    out.push_str(r#"<w:vMerge w:val="restart"/>"#);
                } else if cell.v_merge_continue {
                    out.push_str("<w:vMerge/>");
                }
                if !t.layout_only {
                    out.push_str("<w:tcBorders>");
                    out.push_str(&Self::border_xml("top", &cell.border_top));
                    out.push_str(&Self::border_xml("left", &cell.border_left));
                    out.push_str(&Self::border_xml("bottom", &cell.border_bottom));
                    out.push_str(&Self::border_xml("right", &cell.border_right));
                    out.push_str("</w:tcBorders>");
                }
                if let Some(s) = cell.shading {
                    let _ = write!(out, r#"<w:shd w:val="clear" w:color="auto" w:fill="{}"/>"#, s.hex());
                }
                out.push_str("</w:tcPr>");
                let mut wrote_para = false;
                for b in &cell.blocks {
                    self.flow_block(out, b);
                    wrote_para = !matches!(b, Block::Table(_));
                }
                if !wrote_para {
                    // A cell must end with a paragraph.
                    out.push_str(r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="20" w:lineRule="exact"/><w:rPr><w:sz w:val="2"/></w:rPr></w:pPr></w:p>"#);
                }
                out.push_str("</w:tc>");
            }
            out.push_str("</w:tr>");
        }
        out.push_str("</w:tbl>");
    }

    // ------------------------------------------------------------ dispatch

    fn flow_block(&mut self, out: &mut String, b: &Block) {
        match b {
            Block::Paragraph(p) => self.paragraph(out, p, None, None),
            Block::Table(t) => self.table(out, t, None),
            Block::Picture(p) => self.picture_paragraph(out, p),
            Block::Rule(r) => self.rule(out, r),
        }
    }

    /// Exact mode: everything is positioned absolutely on the page.
    fn exact_block(&mut self, out: &mut String, b: &Block, anchors: &mut Vec<String>) {
        match b {
            Block::Paragraph(p) => {
                let pitch = p.line_pitch.unwrap_or(1.2 * p.font_size);
                let top = p.first_baseline - pitch + DESCENT * p.font_size;
                let height = pitch * p.n_lines as f32;
                // A little slack absorbs font substitution differences.
                let width = p.bbox.width() * 1.03 + p.font_size * 0.6;
                let mut q = p.clone();
                // Inside the frame, x = 0 is the paragraph's leftmost line.
                let container_x0 = p.bbox.x0 - p.indent_left - p.indent_first.min(0.0);
                q.indent_left = 0.0;
                q.indent_right = 0.0;
                if !matches!(p.align, Align::Left | Align::Justify) {
                    q.indent_first = 0.0;
                }
                if p.align == Align::Justify && p.n_lines == 1 {
                    q.align = Align::Left;
                }
                q.tabs = p
                    .tabs
                    .iter()
                    .map(|t| TabStop { pos: t.pos + container_x0 - p.bbox.x0, kind: t.kind, leader: false })
                    .filter(|t| t.pos > 0.0)
                    .collect();
                // Anchor the (slightly wider) frame on the side the text is aligned to.
                let x = match (p.align, p.rtl) {
                    (Align::Center, _) => p.bbox.cx() - width / 2.0,
                    (Align::Right, false) | (Align::Left, true) | (Align::Justify, true) => p.bbox.x1 - width,
                    _ => p.bbox.x0,
                };
                self.paragraph(out, &q, Some((x.max(0.0), top.max(0.0), width, height)), None);
            }
            Block::Table(t) if t.layout_only => {
                for row in &t.rows {
                    for cell in &row.cells {
                        for b in &cell.blocks {
                            self.exact_block(out, b, anchors);
                        }
                    }
                }
            }
            Block::Table(t) => self.table(out, t, Some((t.bbox.x0, t.bbox.y0))),
            Block::Picture(p) => {
                let run = self.anchored_picture_run(p, false);
                anchors.push(run);
            }
            Block::Rule(r) => {
                let x = r.bbox.x0;
                let p = Paragraph {
                    line_pitch: Some(1.0),
                    bbox: r.bbox,
                    ..Default::default()
                };
                let sz = ((r.width * 8.0).round() as i64).clamp(2, 96);
                let mut s = String::new();
                self.paragraph(&mut s, &p, Some((x, r.bbox.y0 - 1.0, r.bbox.width(), 1.0)), None);
                let s = s.replacen(
                    "<w:spacing",
                    &format!(r#"<w:pBdr><w:bottom w:val="single" w:sz="{sz}" w:space="0" w:color="{}"/></w:pBdr><w:spacing"#, r.color.hex()),
                    1,
                );
                out.push_str(&s);
            }
        }
    }

    fn sect_pr(&mut self, page: &PageLayout) -> String {
        let geom = SectGeom {
            width: page.width,
            height: page.height,
            margins: [page.margin_top, page.margin_right, page.margin_bottom, page.margin_left],
            columns: &[],
            continuous: false,
            free_bottom: (page.content_bottom > 0.0).then_some(page.height - page.content_bottom),
            page: Some(page.index),
        };
        self.sect_pr_geom(&geom)
    }

    fn sect_pr_geom(&mut self, g: &SectGeom) -> String {
        let first = self.sections == 0;
        self.sections += 1;
        let orient = if g.width > g.height { r#" w:orient="landscape""# } else { "" };
        let mut refs = self.sect_refs.clone();
        if !first {
            // Only the first section has a distinct first page.
            refs = refs.split("/>").filter(|r| !r.is_empty() && !r.contains(r#"w:type="first""#)).map(|r| format!("{r}/>")).collect();
        }
        // A page without header/footer in the PDF gets blank ones.
        let no_header = g.page.is_some_and(|p| self.no_header.contains(&p)) && !(first && self.title_page);
        let no_footer = g.page.is_some_and(|p| self.no_footer.contains(&p)) && !(first && self.title_page);
        if no_header || no_footer {
            let blank: Vec<&str> = self.blank_refs.split("/>").filter(|r| !r.is_empty()).collect();
            let mut out = String::new();
            for r in refs.split("/>").filter(|r| !r.is_empty()) {
                let header = r.starts_with("<w:headerReference");
                let kind = if header { "header" } else { "footer" };
                if (header && no_header) || (!header && no_footer) {
                    let ty = r.split("w:type=\"").nth(1).and_then(|t| t.split('"').next()).unwrap_or("default");
                    if let Some(b) = blank.iter().find(|b| b.starts_with(&format!("<w:{kind}Reference"))) {
                        out.push_str(&b.replace("w:type=\"default\"", &format!("w:type=\"{ty}\"")));
                        out.push_str("/>");
                        continue;
                    }
                }
                out.push_str(r);
                out.push_str("/>");
            }
            refs = out;
        }
        let kind = if g.continuous && !first { r#"<w:type w:val="continuous"/>"# } else { "" };
        // Each page keeps its printed number (front matter in roman numerals,
        // numbering restarted at a chapter, pages that overflowed).
        let num = match g.page.and_then(|p| self.page_numbers.get(&p)) {
            Some(&(v, roman)) if v >= 0 => format!(r#"<w:pgNumType{} w:start="{v}"/>"#, if roman { r#" w:fmt="lowerRoman""# } else { "" }),
            _ if first && self.first_page_number != 1 => format!(r#"<w:pgNumType w:start="{}"/>"#, self.first_page_number),
            _ => String::new(),
        };
        let title = if first && self.title_page { "<w:titlePg/>" } else { "" };
        let cols = if g.columns.len() > 1 {
            let mut c = format!(r#"<w:cols w:num="{}" w:space="0" w:equalWidth="0">"#, g.columns.len());
            for col in g.columns {
                let _ = write!(c, r#"<w:col w:w="{}" w:space="{}"/>"#, tw(col.width), tw(col.space));
            }
            c.push_str("</w:cols>");
            c
        } else {
            r#"<w:cols w:space="720"/>"#.to_string()
        };
        let [mut mt, mr, mut mb, ml] = g.margins;
        // The margins must hold the header/footer (as Word would grow them):
        // a footer taller than its margin sends LibreOffice into a layout loop.
        // Where that would take room the body needs, the header/footer moves
        // towards the page edge instead.
        let (mut hd, mut fd) = (self.header_distance, self.footer_distance);
        if !(first && self.blank_first.0) && !no_header && self.header_room > 0.0 {
            let height = self.header_room - self.header_distance;
            if self.header_room > mt {
                hd = (mt - height).max(0.0).min(hd);
            }
            mt = mt.max(hd + height);
        }
        if !(first && self.blank_first.1) && !no_footer && self.footer_room > 0.0 {
            let height = self.footer_room - self.footer_distance;
            if let Some(free) = g.free_bottom {
                // Keep most of the slack the body had (font substitution, rounding):
                // the footer moves towards the page edge instead.
                let room = (free - 14.0).min(0.4 * free).max(height + 2.0);
                if self.footer_room > room {
                    fd = (room - height).max(0.0).min(fd);
                }
            }
            mb = mb.max(fd + height);
        }
        format!(
            r#"<w:sectPr>{refs}{kind}<w:pgSz w:w="{}" w:h="{}"{orient}/><w:pgMar w:top="{}" w:right="{}" w:bottom="{}" w:left="{}" w:header="{}" w:footer="{}" w:gutter="0"/>{num}{cols}{title}</w:sectPr>"#,
            // Word refuses pages smaller than about a tenth of an inch.
            tw(g.width).max(144),
            tw(g.height).max(144),
            tw(mt),
            tw(mr),
            tw(mb),
            tw(ml),
            // A header/footer may not start inside the body area: LibreOffice loops on that.
            tw(hd.min(mt)),
            tw(fd.min(mb)),
        )
    }

    /// Continuous mode: one flowing section (Word columns, anchored floats).
    fn flow_section(&mut self, s: &crate::reflow::FlowSection, last: bool) {
        let mut out = String::new();
        for (bi, b) in s.blocks.iter().enumerate() {
            self.anchors_before(&mut out, s, bi);
            self.flow_block(&mut out, b);
        }
        self.anchors_before(&mut out, s, s.blocks.len());
        let geom = SectGeom {
            width: s.width,
            height: s.height,
            margins: [s.margin_top, s.margin_right, s.margin_bottom, s.margin_left],
            columns: &s.columns,
            continuous: !s.new_page,
            free_bottom: None,
            page: None,
        };
        let sect = self.sect_pr_geom(&geom);
        if last {
            out.push_str(&sect.replace("<w:sectPr>", "<w:sectPr_BODY>"));
        } else {
            self.spacer(&mut out, 1.0, Some(&sect));
        }
        self.body.push_str(&out);
    }

    fn anchors_before(&mut self, out: &mut String, s: &crate::reflow::FlowSection, index: usize) {
        let mut runs = String::new();
        for a in s.anchors.iter().filter(|a| a.before == index) {
            let r = match &a.item {
                crate::reflow::AnchorItem::Picture(p) => self.anchored_picture_run_at(p, p.behind_text, a.dy),
                crate::reflow::AnchorItem::TextBox(tb) => self.text_box_run_at(tb, a.dy),
            };
            runs.push_str(&r);
        }
        if !runs.is_empty() {
            let _ = write!(out, r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="20" w:lineRule="exact"/></w:pPr>{runs}</w:p>"#);
        }
    }

    fn page(&mut self, page: &PageLayout, last: bool) {
        let mut out = String::new();
        let sect = self.sect_pr(page);
        let mut anchors: Vec<String> = Vec::new();
        for f in &page.floating {
            let r = self.anchored_picture_run(f, f.behind_text);
            anchors.push(r);
        }
        for tb in &page.text_boxes {
            let r = self.text_box_run(tb);
            anchors.push(r);
        }
        match self.mode {
            LayoutMode::Flow | LayoutMode::Continuous => {
                // Floating pictures are anchored in a zero-height paragraph at the top of the page.
                if !anchors.is_empty() {
                    let _ = write!(
                        out,
                        r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="20" w:lineRule="exact"/></w:pPr>{}</w:p>"#,
                        anchors.join("")
                    );
                    anchors.clear();
                }
                for b in &page.blocks {
                    self.flow_block(&mut out, b);
                }
            }
            LayoutMode::Exact => {
                // Frames attach to the paragraph before them: give every page its own
                // (empty) anchor so frames cannot drift onto the previous page.
                out.push_str(r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="20" w:lineRule="exact"/><w:rPr><w:sz w:val="2"/></w:rPr></w:pPr></w:p>"#);
                for b in &page.blocks {
                    self.exact_block(&mut out, b, &mut anchors);
                }
                if !anchors.is_empty() {
                    let _ = write!(
                        out,
                        r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="20" w:lineRule="exact"/></w:pPr>{}</w:p>"#,
                        anchors.join("")
                    );
                }
            }
        }
        if last {
            // Frames must not be the document's last paragraphs (LibreOffice then
            // lays them out side by side).
            if self.mode == LayoutMode::Exact {
                self.spacer(&mut out, 1.0, None);
            }
            out.push_str(&sect.replace("<w:sectPr>", "<w:sectPr_BODY>"));
        } else {
            self.spacer(&mut out, 1.0, Some(&sect));
        }
        self.body.push_str(&out);
    }
}

const NS: &str = r#"xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" mc:Ignorable="w14 wp14""#;

fn style_rpr(d: &StyleDef, normal: Option<&StyleDef>) -> String {
    let f = esc(&d.font);
    let mut r = format!(r#"<w:rPr><w:rFonts w:ascii="{f}" w:hAnsi="{f}" w:cs="{f}" w:eastAsia="{f}"/>"#);
    let (nb, ni) = normal.map(|n| (n.bold, n.italic)).unwrap_or((false, false));
    if d.bold {
        r.push_str("<w:b/><w:bCs/>");
    } else if nb {
        r.push_str(r#"<w:b w:val="0"/><w:bCs w:val="0"/>"#);
    }
    if d.italic {
        r.push_str("<w:i/><w:iCs/>");
    } else if ni {
        r.push_str(r#"<w:i w:val="0"/><w:iCs w:val="0"/>"#);
    }
    let hp = (d.size * 2.0).round().max(2.0) as i64;
    let _ = write!(r, r#"<w:color w:val="{}"/><w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/></w:rPr>"#, d.color.hex());
    r
}

fn styles_xml(st: &DocStructure, default_font: &str, default_size: f32) -> String {
    let normal = st.styles.iter().find(|d| d.id == "Normal");
    let (font, size) = normal.map(|n| (n.font.as_str(), n.size)).unwrap_or((default_font, default_size));
    let f = esc(font);
    let hp = (size * 2.0).round() as i64;
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="{f}" w:hAnsi="{f}" w:cs="{f}" w:eastAsia="{f}"/><w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/><w:lang w:val="en-US" w:eastAsia="en-US" w:bidi="ar-SA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>"#
    );
    match normal {
        Some(n) => {
            let _ = write!(s, r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/>{}</w:style>"#, style_rpr(n, None));
        }
        None => s.push_str(r#"<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>"#),
    }
    s.push_str(r#"<w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:semiHidden/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="0" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="0" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style><w:style w:type="numbering" w:default="1" w:styleId="NoList"><w:name w:val="No List"/><w:uiPriority w:val="99"/><w:semiHidden/></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/></w:style>"#);
    for lvl in 1..=6u8 {
        let id = format!("Heading{lvl}");
        let rpr = st.styles.iter().find(|d| d.id == id).map(|d| style_rpr(d, normal)).unwrap_or_default();
        let _ = write!(
            s,
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="heading {lvl}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:outlineLvl w:val="{}"/></w:pPr>{rpr}</w:style>"#,
            lvl - 1
        );
    }
    for d in &st.styles {
        if d.id == "Normal" || d.id.starts_with("Heading") {
            continue;
        }
        let _ = write!(
            s,
            r#"<w:style w:type="paragraph" w:styleId="{}"><w:name w:val="{}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/>{}</w:style>"#,
            esc(&d.id),
            esc(&d.name),
            style_rpr(d, normal)
        );
    }
    if !st.footnotes.is_empty() {
        if !st.styles.iter().any(|d| d.id == "FootnoteText") {
            let hp = ((size * 0.8) * 2.0).round() as i64;
            let _ = write!(s, r#"<w:style w:type="paragraph" w:styleId="FootnoteText"><w:name w:val="footnote text"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/><w:rPr><w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/></w:rPr></w:style>"#);
        }
        s.push_str(r#"<w:style w:type="character" w:styleId="FootnoteReference"><w:name w:val="footnote reference"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:rPr><w:vertAlign w:val="superscript"/></w:rPr></w:style>"#);
    }
    if !st.comments.is_empty() {
        s.push_str(r#"<w:style w:type="character" w:styleId="CommentReference"><w:name w:val="annotation reference"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/><w:semiHidden/><w:rPr><w:sz w:val="16"/><w:szCs w:val="16"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="CommentText"><w:name w:val="annotation text"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/><w:rPr><w:sz w:val="20"/><w:szCs w:val="20"/></w:rPr></w:style>"#);
    }
    if st.header.is_some() || st.footer.is_some() {
        s.push_str(r#"<w:style w:type="paragraph" w:styleId="Header"><w:name w:val="header"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/></w:style><w:style w:type="paragraph" w:styleId="Footer"><w:name w:val="footer"/><w:basedOn w:val="Normal"/><w:uiPriority w:val="99"/></w:style>"#);
    }
    s.push_str("</w:styles>");
    s
}

fn num_fmt(f: NumFormat) -> &'static str {
    match f {
        NumFormat::Bullet => "bullet",
        NumFormat::Decimal => "decimal",
        NumFormat::LowerLetter => "lowerLetter",
        NumFormat::UpperLetter => "upperLetter",
        NumFormat::LowerRoman => "lowerRoman",
        NumFormat::UpperRoman => "upperRoman",
    }
}

/// numbering.xml: one abstract numbering (and one instance) per detected list.
fn numbering_xml(lists: &[ListDef]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    );
    for (i, l) in lists.iter().enumerate() {
        let _ = write!(s, r#"<w:abstractNum w:abstractNumId="{i}"><w:multiLevelType w:val="multilevel"/>"#);
        // Word expects all nine levels; undefined ones continue the last.
        for lvl in 0..9usize {
            let def = l.levels.get(lvl);
            let last = l.levels.last();
            let (fmt, text, start, indent, hanging) = match def {
                Some(d) => (d.format, d.text.clone(), d.start, d.indent_left, d.hanging),
                None => {
                    let extra = (lvl + 1 - l.levels.len()) as f32 * 18.0;
                    let (ind, hang) = last.map(|d| (d.indent_left + extra, d.hanging)).unwrap_or((18.0 * (lvl + 1) as f32, 18.0));
                    (NumFormat::Decimal, format!("%{}.", lvl + 1), 1, ind, hang)
                }
            };
            let text = if text.is_empty() { if fmt == NumFormat::Bullet { "•".to_string() } else { format!("%{}.", lvl + 1) } } else { text };
            let _ = write!(
                s,
                r#"<w:lvl w:ilvl="{lvl}"><w:start w:val="{}"/><w:numFmt w:val="{}"/><w:lvlText w:val="{}"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{}" w:hanging="{}"/></w:pPr>"#,
                start.clamp(1, 32767),
                num_fmt(fmt),
                esc(&text),
                tw(indent.max(0.0)),
                tw(hanging.max(0.0))
            );
            if let Some(d) = def {
                let m = &d.marker_style;
                let lower = m.font.to_lowercase();
                s.push_str("<w:rPr>");
                // Symbol fonts map bullets to private code points: let the bullet use the text font.
                if !["symbol", "dingbat", "wingding"].iter().any(|k| lower.contains(k)) && !m.font.is_empty() {
                    let f = esc(&m.font);
                    let _ = write!(s, r#"<w:rFonts w:ascii="{f}" w:hAnsi="{f}" w:cs="{f}" w:hint="default"/>"#);
                }
                if m.bold {
                    s.push_str("<w:b/><w:bCs/>");
                }
                if m.italic {
                    s.push_str("<w:i/><w:iCs/>");
                }
                if m.color != Rgb::BLACK {
                    let _ = write!(s, r#"<w:color w:val="{}"/>"#, m.color.hex());
                }
                if m.size > 0.0 {
                    let hp = (m.size * 2.0).round().max(2.0) as i64;
                    let _ = write!(s, r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#);
                }
                s.push_str("</w:rPr>");
            }
            s.push_str("</w:lvl>");
        }
        s.push_str("</w:abstractNum>");
    }
    for i in 0..lists.len() {
        let _ = write!(s, r#"<w:num w:numId="{}"><w:abstractNumId w:val="{i}"/></w:num>"#, i + 1);
    }
    s.push_str("</w:numbering>");
    s
}

fn rels_xml(rels: &[Rel], fixed: &str) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    s.push_str(fixed);
    for (id, ty, target, external) in rels {
        let mode = if *external { r#" TargetMode="External""# } else { "" };
        let _ = write!(s, r#"<Relationship Id="{id}" Type="{ty}" Target="{}"{mode}/>"#, esc(target));
    }
    s.push_str("</Relationships>");
    s
}

const REL_HEADER: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const REL_FOOTER: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
const REL_FOOTNOTES: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
const REL_COMMENTS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const REL_NUMBERING: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";

const SETTINGS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:zoom w:percent="100"/>EMBED<w:defaultTabStop w:val="720"/>EVENODD<w:characterSpacingControl w:val="doNotCompress"/>FOOTNOTEPR<w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/><w:compatSetting w:name="overrideTableStyleFontSizeAndJustification" w:uri="http://schemas.microsoft.com/office/word" w:val="1"/><w:compatSetting w:name="enableOpenTypeFeatures" w:uri="http://schemas.microsoft.com/office/word" w:val="1"/><w:compatSetting w:name="doNotFlipMirrorIndents" w:uri="http://schemas.microsoft.com/office/word" w:val="1"/></w:compat></w:settings>"#;

fn content_types(has: &[&str], overrides: &[(String, &str)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>"#,
    );
    if has.contains(&"png") {
        s.push_str(r#"<Default Extension="png" ContentType="image/png"/>"#);
    }
    if has.contains(&"jpeg") {
        s.push_str(r#"<Default Extension="jpeg" ContentType="image/jpeg"/>"#);
    }
    if has.contains(&"odttf") {
        s.push_str(r#"<Default Extension="odttf" ContentType="application/vnd.openxmlformats-officedocument.obfuscatedFont"/>"#);
    }
    for (part, ty) in overrides {
        let _ = write!(s, r#"<Override PartName="{part}" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.{ty}+xml"/>"#);
    }
    s.push_str(r#"<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/><Override PartName="/word/fontTable.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.fontTable+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>"#);
    s
}

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#;

fn core_xml(meta: &DocMeta) -> String {
    let opt = |tag: &str, v: &Option<String>| v.as_ref().map(|v| format!("<{tag}>{}</{tag}>", esc(v))).unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">{}{}{}{}</cp:coreProperties>"#,
        opt("dc:title", &meta.title),
        opt("dc:subject", &meta.subject),
        opt("dc:creator", &meta.author),
        opt("cp:keywords", &meta.keywords),
    )
}

const APP_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>pdf2word</Application></Properties>"#;

/// Serialises the analysed pages to a DOCX package.
pub fn write_docx<W: Write + Seek>(out: W, pages: &[PageLayout], meta: &DocMeta, mode: LayoutMode) -> anyhow::Result<()> {
    let mut w = Writer::new(mode);
    let st = &meta.structure;
    w.styles = st.styles.iter().map(|d| (d.id.clone(), d.clone())).collect();
    for d in &st.styles {
        w.fonts.entry(d.font.clone()).or_insert_with(|| crate::fonts::generic_of(&d.font));
    }
    w.first_page_number = meta.first_page_number.max(1);
    w.title_page = st.blank_first_header || st.blank_first_footer;
    if mode != LayoutMode::Continuous {
        w.page_numbers = st.page_numbers.clone();
    }
    w.blank_first = (st.blank_first_header, st.blank_first_footer);

    // Headers and footers: separate parts referenced from every section.
    let mut parts: Vec<(String, String, Vec<Rel>)> = Vec::new(); // (name, xml, rels)
    let mut overrides: Vec<(String, &str)> = Vec::new();
    for (kind, hf, even) in [("header", &st.header, &st.header_even), ("footer", &st.footer, &st.footer_even)] {
        let Some(hf) = hf else { continue };
        let (tag, rel) = if kind == "header" { ("hdr", REL_HEADER) } else { ("ftr", REL_FOOTER) };
        let write_part = |w: &mut Writer, hf: &HeaderFooter| -> (String, Vec<Rel>) {
            let (body, rels) = w.part(|w, out| {
                for b in &hf.blocks {
                    w.flow_block(out, b);
                }
                if !matches!(hf.blocks.last(), Some(Block::Paragraph(_) | Block::Picture(_) | Block::Rule(_))) {
                    // Must end with a paragraph: keep it from adding a line.
                    w.spacer(out, 1.0, None);
                }
            });
            (format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:{tag} {NS}>{body}</w:{tag}>"), rels)
        };
        let (xml, rels) = write_part(&mut w, hf);
        let name = format!("{kind}1.xml");
        let id = w.add_rel(rel, name.clone(), false);
        let _ = write!(w.sect_refs, r#"<w:{kind}Reference w:type="default" r:id="{id}"/>"#);
        parts.push((name.clone(), xml.clone(), rels.clone()));
        overrides.push((format!("/word/{name}"), kind));
        if w.title_page {
            // A distinct first page: empty where the PDF's first page lacks it.
            let blank = if kind == "header" { st.blank_first_header } else { st.blank_first_footer };
            let name = format!("{kind}2.xml");
            let id = w.add_rel(rel, name.clone(), false);
            let _ = write!(w.sect_refs, r#"<w:{kind}Reference w:type="first" r:id="{id}"/>"#);
            if blank {
                parts.push((name.clone(), format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:{tag} {NS}><w:p/></w:{tag}>"), Vec::new()));
            } else {
                parts.push((name.clone(), xml, rels));
            }
            overrides.push((format!("/word/{name}"), kind));
        }
        let without = if kind == "header" { &st.pages_without_header } else { &st.pages_without_footer };
        if !without.is_empty() && mode != LayoutMode::Continuous {
            let name = format!("{kind}0.xml");
            let id = w.add_rel(rel, name.clone(), false);
            let _ = write!(w.blank_refs, r#"<w:{kind}Reference w:type="default" r:id="{id}"/>"#);
            parts.push((name.clone(), format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:{tag} {NS}><w:p/></w:{tag}>"), Vec::new()));
            overrides.push((format!("/word/{name}"), kind));
            let set = if kind == "header" { &mut w.no_header } else { &mut w.no_footer };
            set.extend(without.iter().copied());
        }
        let mut distance = hf.distance;
        let mut room = hf.distance + hf.height + 2.0;
        if let Some(e) = even {
            let (xml, rels) = write_part(&mut w, e);
            let name = format!("{kind}3.xml");
            let id = w.add_rel(rel, name.clone(), false);
            let _ = write!(w.sect_refs, r#"<w:{kind}Reference w:type="even" r:id="{id}"/>"#);
            parts.push((name.clone(), xml, rels));
            overrides.push((format!("/word/{name}"), kind));
            distance = distance.min(e.distance);
            room = room.max(e.distance + e.height + 2.0);
        }
        if kind == "header" {
            w.header_distance = distance;
            w.header_room = room;
        } else {
            w.footer_distance = distance;
            w.footer_room = room;
        }
    }
    // Header references must precede footer references.
    let mut refs: Vec<String> = w.sect_refs.split("/>").filter(|r| !r.is_empty()).map(|r| format!("{r}/>")).collect();
    refs.sort_by_key(|r| !r.starts_with("<w:headerReference"));
    w.sect_refs = refs.concat();

    // Footnotes.
    if !st.footnotes.is_empty() {
        let (body, rels) = w.part(|w, out| {
            out.push_str(r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"#);
            for (i, note) in st.footnotes.iter().enumerate() {
                let mut x = String::new();
                for b in &note.blocks {
                    w.flow_block(&mut x, b);
                }
                let mark = r#"<w:r><w:rPr><w:rStyle w:val="FootnoteReference"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r>"#;
                let x = if x.starts_with("<w:p><w:pPr>") {
                    x.replacen("</w:pPr>", &format!("</w:pPr>{mark}"), 1)
                } else {
                    format!(r#"<w:p><w:pPr><w:pStyle w:val="FootnoteText"/></w:pPr>{mark}</w:p>{x}"#)
                };
                let _ = write!(out, r#"<w:footnote w:id="{}">{x}</w:footnote>"#, i + 1);
            }
        });
        w.add_rel(REL_FOOTNOTES, "footnotes.xml".into(), false);
        parts.push(("footnotes.xml".into(), format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:footnotes {NS}>{body}</w:footnotes>"), rels));
        overrides.push(("/word/footnotes.xml".into(), "footnotes"));
    }
    if !st.comments.is_empty() {
        w.add_rel(REL_COMMENTS, "comments.xml".into(), false);
        let mut xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:comments {NS}>");
        for (i, c) in st.comments.iter().enumerate() {
            let author = if c.author.is_empty() { "PDF" } else { c.author.as_str() };
            let initials: String = author.split_whitespace().filter_map(|w| w.chars().next()).take(3).collect();
            let date = c.date.as_ref().map(|d| format!(r#" w:date="{}""#, esc(d))).unwrap_or_default();
            let _ = write!(xml, r#"<w:comment w:id="{i}" w:author="{}"{date} w:initials="{}">"#, esc(author), esc(&initials));
            for (k, line) in c.text.split('\n').enumerate() {
                xml.push_str(r#"<w:p><w:pPr><w:pStyle w:val="CommentText"/></w:pPr>"#);
                if k == 0 {
                    xml.push_str(r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:annotationRef/></w:r>"#);
                }
                let _ = write!(xml, r#"<w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#, esc(line));
            }
            xml.push_str("</w:comment>");
        }
        xml.push_str("</w:comments>");
        parts.push(("comments.xml".into(), xml, Vec::new()));
        overrides.push(("/word/comments.xml".into(), "comments"));
    }
    if !st.lists.is_empty() {
        w.add_rel(REL_NUMBERING, "numbering.xml".into(), false);
        parts.push(("numbering.xml".into(), numbering_xml(&st.lists), Vec::new()));
        overrides.push(("/word/numbering.xml".into(), "numbering"));
    }

    if pages.is_empty() {
        let blank = PageLayout {
            width: 612.0,
            height: 792.0,
            margin_left: 72.0,
            margin_right: 72.0,
            margin_top: 72.0,
            margin_bottom: 72.0,
            ..Default::default()
        };
        w.page(&blank, true);
    }
    if mode == LayoutMode::Continuous && !pages.is_empty() {
        let sections = crate::reflow::build(pages.to_vec());
        for (i, s) in sections.iter().enumerate() {
            w.flow_section(s, i + 1 == sections.len());
        }
    } else {
        for (i, p) in pages.iter().enumerate() {
            w.page(p, i + 1 == pages.len());
        }
    }

    // Default font: most used family.
    let default_font = w.fonts.keys().next().cloned().unwrap_or_else(|| "Times New Roman".into());
    let body = std::mem::take(&mut w.body).replace("<w:sectPr_BODY>", "<w:sectPr>");
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {NS}><w:body>{body}</w:body></w:document>"#
    );

    let doc_rels = rels_xml(
        &w.rels,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable" Target="fontTable.xml"/>"#,
    );

    let mut font_table = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:fonts xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
    );
    // Embedded fonts: obfuscated .odttf parts referenced from the font table.
    let mut font_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    let mut font_parts: Vec<(String, Vec<u8>)> = Vec::new();
    let mut embeds: std::collections::BTreeMap<String, String> = Default::default();
    for (i, f) in meta.fonts.iter().enumerate() {
        if !w.fonts.contains_key(&f.family) {
            continue;
        }
        let key = crate::embed::font_key(&f.data, i);
        let name = format!("font{}.odttf", i + 1);
        let rid = format!("rId{}", i + 1);
        let _ = write!(
            font_rels,
            r#"<Relationship Id="{rid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/font" Target="fonts/{name}"/>"#
        );
        let tag = match (f.bold, f.italic) {
            (false, false) => "embedRegular",
            (true, false) => "embedBold",
            (false, true) => "embedItalic",
            (true, true) => "embedBoldItalic",
        };
        let order = match tag {
            "embedRegular" => 0,
            "embedBold" => 1,
            "embedItalic" => 2,
            _ => 3,
        };
        let entry = embeds.entry(f.family.clone()).or_default();
        entry.push_str(&format!("{order}<w:{tag} r:id=\"{rid}\" w:fontKey=\"{key}\"/>\n"));
        font_parts.push((name, crate::embed::obfuscate(&f.data, &key)));
    }
    font_rels.push_str("</Relationships>");
    for (f, generic) in &w.fonts {
        // Family class and pitch steer Word's substitution when the font is missing.
        let (family, pitch) = match generic {
            Generic::Serif => ("roman", "variable"),
            Generic::Sans => ("swiss", "variable"),
            Generic::Mono => ("modern", "fixed"),
            Generic::Unknown => ("auto", "variable"),
        };
        let mut embed_xml = String::new();
        if let Some(list) = embeds.get(f) {
            let mut items: Vec<&str> = list.lines().collect();
            items.sort();
            for it in items {
                embed_xml.push_str(&it[1..]);
            }
        }
        let _ = write!(font_table, r#"<w:font w:name="{}"><w:family w:val="{family}"/><w:pitch w:val="{pitch}"/>{embed_xml}</w:font>"#, esc(f));
    }
    font_table.push_str("</w:fonts>");

    let mut exts: Vec<&str> = w.media.iter().map(|m| if m.name.ends_with(".png") { "png" } else { "jpeg" }).collect();
    if !font_parts.is_empty() {
        exts.push("odttf");
    }
    let settings = SETTINGS
        .replace("EMBED", if font_parts.is_empty() { "" } else { "<w:embedTrueTypeFonts/><w:saveSubsetFonts/>" })
        .replace("EVENODD", if st.header_even.is_some() || st.footer_even.is_some() { "<w:evenAndOddHeaders/>" } else { "" })
        .replace(
            "FOOTNOTEPR",
            if st.footnotes.is_empty() { "" } else { r#"<w:footnotePr><w:footnote w:id="-1"/><w:footnote w:id="0"/></w:footnotePr>"# },
        );

    let mut zip = ZipWriter::new(out);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("[Content_Types].xml", deflate)?;
    zip.write_all(content_types(&exts, &overrides).as_bytes())?;
    zip.start_file("_rels/.rels", deflate)?;
    zip.write_all(ROOT_RELS.as_bytes())?;
    zip.start_file("docProps/core.xml", deflate)?;
    zip.write_all(core_xml(meta).as_bytes())?;
    zip.start_file("docProps/app.xml", deflate)?;
    zip.write_all(APP_XML.as_bytes())?;
    zip.start_file("word/document.xml", deflate)?;
    zip.write_all(document.as_bytes())?;
    zip.start_file("word/styles.xml", deflate)?;
    zip.write_all(styles_xml(st, &default_font, 11.0).as_bytes())?;
    zip.start_file("word/settings.xml", deflate)?;
    zip.write_all(settings.as_bytes())?;
    zip.start_file("word/fontTable.xml", deflate)?;
    zip.write_all(font_table.as_bytes())?;
    if !font_parts.is_empty() {
        zip.start_file("word/_rels/fontTable.xml.rels", deflate)?;
        zip.write_all(font_rels.as_bytes())?;
        for (name, data) in &font_parts {
            zip.start_file(format!("word/fonts/{name}"), deflate)?;
            zip.write_all(data)?;
        }
    }
    zip.start_file("word/_rels/document.xml.rels", deflate)?;
    zip.write_all(doc_rels.as_bytes())?;
    for (name, xml, rels) in &parts {
        zip.start_file(format!("word/{name}"), deflate)?;
        zip.write_all(xml.as_bytes())?;
        if !rels.is_empty() {
            zip.start_file(format!("word/_rels/{name}.rels"), deflate)?;
            zip.write_all(rels_xml(rels, "").as_bytes())?;
        }
    }
    for m in &w.media {
        zip.start_file(format!("word/media/{}", m.name), stored)?;
        zip.write_all(&m.bytes)?;
    }
    zip.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(esc("a<b>&\"c'\u{1}"), "a&lt;b&gt;&amp;&quot;c&apos;");
    }

    #[test]
    fn writes_valid_zip() {
        let p = Paragraph {
            inlines: vec![Inline::Text { text: "Hello & world".into(), style: TextStyle::default(), link: Some("https://example.com".into()), pdf_width: 0.0, pdf_tracking: 0.0 }],
            line_pitch: Some(13.2),
            n_lines: 1,
            font_size: 11.0,
            first_baseline: 80.0,
            bbox: Rect::new(72.0, 70.0, 200.0, 84.0),
            ..Default::default()
        };
        let page = PageLayout { width: 612.0, height: 792.0, blocks: vec![Block::Paragraph(p)], ..Default::default() };
        for mode in [LayoutMode::Flow, LayoutMode::Exact] {
            let mut buf = std::io::Cursor::new(Vec::new());
            write_docx(&mut buf, &[page.clone(), page.clone()], &DocMeta::default(), mode).unwrap();
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(buf.into_inner())).unwrap();
            let mut doc = String::new();
            std::io::Read::read_to_string(&mut z.by_name("word/document.xml").unwrap(), &mut doc).unwrap();
            assert!(doc.contains("Hello &amp; world"));
            assert!(doc.contains("<w:hyperlink"));
            assert_eq!(doc.matches("<w:sectPr>").count(), 2);
            assert!(!doc.contains("sectPr_BODY"));
        }
    }
}
