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
}

#[derive(Clone, Debug, Default)]
pub struct DocMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    /// Fonts to embed into the package.
    pub fonts: Vec<crate::embed::EmbeddedFont>,
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

struct Writer {
    mode: LayoutMode,
    body: String,
    media: Vec<Media>,
    /// (rId, type, target, external)
    rels: Vec<(String, &'static str, String, bool)>,
    next_rel: usize,
    next_drawing: usize,
    fonts: std::collections::BTreeMap<String, Generic>,
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
        }
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
        let f = esc(&st.font);
        let _ = write!(r, r#"<w:rFonts w:ascii="{f}" w:hAnsi="{f}" w:cs="{f}" w:eastAsia="{f}"/>"#);
        if st.bold {
            r.push_str("<w:b/><w:bCs/>");
        }
        if st.italic {
            r.push_str("<w:i/><w:iCs/>");
        }
        if st.small_caps {
            r.push_str("<w:smallCaps/>");
        }
        if st.strike {
            r.push_str("<w:strike/>");
        }
        if st.color != Rgb::BLACK {
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
        let hp = (st.size * 2.0).round().max(2.0) as i64;
        let _ = write!(r, r#"<w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/>"#);
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

    fn inlines(&mut self, out: &mut String, inlines: &[Inline]) {
        let exact = self.mode == LayoutMode::Exact;
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
                        let id = self.add_rel(REL_LINK, url.clone(), true);
                        let _ = write!(out, r#"<w:hyperlink r:id="{id}" w:history="1">"#);
                        // Group consecutive runs with the same link.
                        let mut j = i;
                        while j < inlines.len() {
                            match &inlines[j] {
                                Inline::Text { text, style, link: Some(l), .. } if l == url => {
                                    self.text_run(out, text, style);
                                    last_style = Some(style.clone());
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
                Inline::Tab => {
                    let rpr = last_style.clone().map(|s| self.run_props(&s)).unwrap_or_default();
                    let _ = write!(out, "<w:r>{rpr}<w:tab/></w:r>");
                }
                Inline::LineBreak => out.push_str("<w:r><w:br/></w:r>"),
                Inline::SoftBreak(j) => {
                    let st = last_style.clone().unwrap_or_default();
                    if exact {
                        if *j == Joiner::Hyphen {
                            self.text_run(out, "-", &st);
                        }
                        let rpr = self.run_props(&st);
                        let _ = write!(out, "<w:r>{rpr}<w:br/></w:r>");
                    } else if *j == Joiner::Space {
                        self.text_run(out, " ", &st);
                    }
                }
            }
            i += 1;
        }
    }

    // ------------------------------------------------------------ paragraphs

    #[allow(clippy::too_many_arguments)]
    fn paragraph(&mut self, out: &mut String, p: &Paragraph, frame: Option<(f32, f32, f32, f32)>, sect: Option<&str>) {
        out.push_str("<w:p><w:pPr>");
        if let Some(h) = p.heading {
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
        if let Some(shd) = p.shading {
            let _ = write!(out, r#"<w:shd w:val="clear" w:color="auto" w:fill="{}"/>"#, shd.hex());
        }
        if p.rtl {
            out.push_str("<w:bidi/>");
        }
        if !p.tabs.is_empty() {
            out.push_str("<w:tabs>");
            for t in &p.tabs {
                let kind = match t.kind {
                    TabKind::Left => "left",
                    TabKind::Right => "right",
                    TabKind::Center => "center",
                };
                let _ = write!(out, r#"<w:tab w:val="{kind}" w:pos="{}"/>"#, tw(t.pos.max(0.0)));
            }
            out.push_str("</w:tabs>");
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
        self.inlines(out, &p.inlines);
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

    fn inline_picture_run(&mut self, pic: &Picture) -> String {
        let id = self.next_drawing;
        self.next_drawing += 1;
        let (cx, cy) = (emu(pic.width.max(1.0)), emu(pic.height.max(1.0)));
        let g = self.graphic_xml(pic, id);
        format!(
            r#"<w:r><w:drawing><wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="{id}" name="Picture {id}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr>{g}</wp:inline></w:drawing></w:r>"#
        )
    }

    fn anchored_picture_run(&mut self, pic: &Picture, behind: bool) -> String {
        let id = self.next_drawing;
        self.next_drawing += 1;
        let (cx, cy) = (emu(pic.width.max(1.0)), emu(pic.height.max(1.0)));
        let g = self.graphic_xml(pic, id);
        let wrap = if behind { "<wp:wrapNone/>".to_string() } else { r#"<wp:wrapSquare wrapText="bothSides"/>"#.to_string() };
        format!(
            r#"<w:r><w:drawing><wp:anchor distT="0" distB="0" distL="57150" distR="57150" simplePos="0" relativeHeight="{rh}" behindDoc="{bd}" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="page"><wp:posOffset>{x}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="page"><wp:posOffset>{y}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/>{wrap}<wp:docPr id="{id}" name="Picture {id}"/><wp:cNvGraphicFramePr><a:graphicFrameLocks xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" noChangeAspect="1"/></wp:cNvGraphicFramePr>{g}</wp:anchor></w:drawing></w:r>"#,
            rh = 251658240 + id,
            bd = if behind { 1 } else { 0 },
            x = emu(pic.bbox.x0),
            y = emu(pic.bbox.y0),
        )
    }

    /// A text box anchored at its page position (rotated or vertical text).
    fn text_box_run(&mut self, tb: &TextBox) -> String {
        let id = self.next_drawing;
        self.next_drawing += 1;
        let (w, h) = (tb.width.max(1.0), tb.height.max(1.0));
        let (cx, cy) = (emu(w), emu(h));
        let mut content = String::from(
            r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="240" w:lineRule="auto"/><w:jc w:val="center"/></w:pPr>"#,
        );
        self.inlines(&mut content, &tb.inlines);
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
            r#"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor distT="0" distB="0" distL="0" distR="0" simplePos="0" relativeHeight="{rh}" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:simplePos x="0" y="0"/><wp:positionH relativeFrom="page"><wp:posOffset>{x}</wp:posOffset></wp:positionH><wp:positionV relativeFrom="page"><wp:posOffset>{y}</wp:posOffset></wp:positionV><wp:extent cx="{cx}" cy="{cy}"/><wp:effectExtent l="0" t="0" r="0" b="0"/><wp:wrapNone/><wp:docPr id="{id}" name="Text Box {id}"/><wp:cNvGraphicFramePr/><a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm rot="{rot}"><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></wps:spPr><wps:txbx><w:txbxContent>{content}</w:txbxContent></wps:txbx><wps:bodyPr rot="0" vert="{vert}" wrap="none" lIns="0" tIns="0" rIns="0" bIns="0" anchor="ctr" anchorCtr="0"><a:noAutofit/></wps:bodyPr></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice></mc:AlternateContent></w:r>"#,
            rh = 251660000 + id,
            x = emu(tb.cx - w / 2.0),
            y = emu(tb.cy - h / 2.0),
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
        for row in &t.rows {
            // Layout tables (columns) may break across pages; data rows stay together.
            out.push_str(if t.layout_only { "<w:tr><w:trPr>" } else { "<w:tr><w:trPr><w:cantSplit/>" });
            if row.height > 0.0 && !t.layout_only {
                let _ = write!(out, r#"<w:trHeight w:val="{}" w:hRule="atLeast"/>"#, tw(row.height));
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
                    .map(|t| TabStop { pos: t.pos + container_x0 - p.bbox.x0, kind: t.kind })
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

    fn sect_pr(page: &PageLayout) -> String {
        let orient = if page.width > page.height { r#" w:orient="landscape""# } else { "" };
        format!(
            r#"<w:sectPr><w:pgSz w:w="{}" w:h="{}"{orient}/><w:pgMar w:top="{}" w:right="{}" w:bottom="{}" w:left="{}" w:header="0" w:footer="0" w:gutter="0"/><w:cols w:space="720"/></w:sectPr>"#,
            // Word refuses pages smaller than about a tenth of an inch.
            tw(page.width).max(144),
            tw(page.height).max(144),
            tw(page.margin_top),
            tw(page.margin_right),
            tw(page.margin_bottom),
            tw(page.margin_left)
        )
    }

    fn page(&mut self, page: &PageLayout, last: bool) {
        let mut out = String::new();
        let sect = Self::sect_pr(page);
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
            LayoutMode::Flow => {
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
            out.push_str(&sect.replace("<w:sectPr>", "<w:sectPr_BODY>"));
        } else {
            self.spacer(&mut out, 1.0, Some(&sect));
        }
        self.body.push_str(&out);
    }
}

const NS: &str = r#"xmlns:wpc="http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006" xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:wp14="http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:w10="urn:schemas-microsoft-com:office:word" xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:w14="http://schemas.microsoft.com/office/word/2010/wordml" xmlns:wpg="http://schemas.microsoft.com/office/word/2010/wordprocessingGroup" xmlns:wpi="http://schemas.microsoft.com/office/word/2010/wordprocessingInk" xmlns:wne="http://schemas.microsoft.com/office/word/2006/wordml" xmlns:wps="http://schemas.microsoft.com/office/word/2010/wordprocessingShape" mc:Ignorable="w14 wp14""#;

fn styles_xml(default_font: &str, default_size: f32) -> String {
    let f = esc(default_font);
    let hp = (default_size * 2.0).round() as i64;
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="{f}" w:hAnsi="{f}" w:cs="{f}" w:eastAsia="{f}"/><w:sz w:val="{hp}"/><w:szCs w:val="{hp}"/><w:lang w:val="en-US" w:eastAsia="en-US" w:bidi="ar-SA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="0" w:line="240" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style><w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:name w:val="Default Paragraph Font"/><w:uiPriority w:val="1"/><w:semiHidden/></w:style><w:style w:type="table" w:default="1" w:styleId="TableNormal"><w:name w:val="Normal Table"/><w:semiHidden/><w:tblPr><w:tblInd w:w="0" w:type="dxa"/><w:tblCellMar><w:top w:w="0" w:type="dxa"/><w:left w:w="0" w:type="dxa"/><w:bottom w:w="0" w:type="dxa"/><w:right w:w="0" w:type="dxa"/></w:tblCellMar></w:tblPr></w:style><w:style w:type="character" w:styleId="Hyperlink"><w:name w:val="Hyperlink"/><w:basedOn w:val="DefaultParagraphFont"/><w:uiPriority w:val="99"/></w:style>"#
    );
    for lvl in 1..=3 {
        let _ = write!(
            s,
            r#"<w:style w:type="paragraph" w:styleId="Heading{lvl}"><w:name w:val="heading {lvl}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:uiPriority w:val="9"/><w:qFormat/><w:pPr><w:keepNext/><w:keepLines/><w:outlineLvl w:val="{}"/></w:pPr></w:style>"#,
            lvl - 1
        );
    }
    s.push_str("</w:styles>");
    s
}

const SETTINGS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:zoom w:percent="100"/>EMBED<w:defaultTabStop w:val="720"/><w:characterSpacingControl w:val="doNotCompress"/><w:compat><w:compatSetting w:name="compatibilityMode" w:uri="http://schemas.microsoft.com/office/word" w:val="15"/><w:compatSetting w:name="overrideTableStyleFontSizeAndJustification" w:uri="http://schemas.microsoft.com/office/word" w:val="1"/><w:compatSetting w:name="enableOpenTypeFeatures" w:uri="http://schemas.microsoft.com/office/word" w:val="1"/><w:compatSetting w:name="doNotFlipMirrorIndents" w:uri="http://schemas.microsoft.com/office/word" w:val="1"/></w:compat></w:settings>"#;

fn content_types(has: &[&str]) -> String {
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
    for (i, p) in pages.iter().enumerate() {
        w.page(p, i + 1 == pages.len());
    }

    // Default font: most used family.
    let default_font = w.fonts.keys().next().cloned().unwrap_or_else(|| "Times New Roman".into());
    let body = std::mem::take(&mut w.body).replace("<w:sectPr_BODY>", "<w:sectPr>");
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document {NS}><w:body>{body}</w:body></w:document>"#
    );

    let mut doc_rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings" Target="settings.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable" Target="fontTable.xml"/>"#,
    );
    for (id, ty, target, external) in &w.rels {
        let mode = if *external { r#" TargetMode="External""# } else { "" };
        let _ = write!(doc_rels, r#"<Relationship Id="{id}" Type="{ty}" Target="{}"{mode}/>"#, esc(target));
    }
    doc_rels.push_str("</Relationships>");

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
    let settings = SETTINGS.replace(
        "EMBED",
        if font_parts.is_empty() { "" } else { "<w:embedTrueTypeFonts/><w:saveSubsetFonts/>" },
    );

    let mut zip = ZipWriter::new(out);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("[Content_Types].xml", deflate)?;
    zip.write_all(content_types(&exts).as_bytes())?;
    zip.start_file("_rels/.rels", deflate)?;
    zip.write_all(ROOT_RELS.as_bytes())?;
    zip.start_file("docProps/core.xml", deflate)?;
    zip.write_all(core_xml(meta).as_bytes())?;
    zip.start_file("docProps/app.xml", deflate)?;
    zip.write_all(APP_XML.as_bytes())?;
    zip.start_file("word/document.xml", deflate)?;
    zip.write_all(document.as_bytes())?;
    zip.start_file("word/styles.xml", deflate)?;
    zip.write_all(styles_xml(&default_font, 11.0).as_bytes())?;
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
