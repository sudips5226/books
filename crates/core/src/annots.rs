//! Document navigation and interactive content: the outline (bookmarks),
//! link destinations, review annotations and form fields.

use pdfium_render::prelude::*;

use crate::extract::PageTransform;
use crate::model::*;

/// One entry of the PDF outline.
#[derive(Clone, Debug)]
pub struct OutlineEntry {
    pub title: String,
    /// 0 = top level.
    pub level: usize,
    pub page: usize,
    /// Target position on the page (points from the top), if given.
    pub y: Option<f32>,
}

/// Page index and (x, y) position (in that page's top-down coordinates) of a destination.
pub fn dest_target(document: &PdfDocument, dest: &PdfDestination) -> Option<(usize, Option<f32>, Option<f32>)> {
    let page = dest.page_index().ok()? as usize;
    let (x, y) = match dest.view_settings().ok()? {
        PdfDestinationViewSettings::SpecificCoordinatesAndZoom(x, y, _) => (x.map(|v| v.value), y.map(|v| v.value)),
        PdfDestinationViewSettings::FitPageHorizontallyToWindow(y) | PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(y) => (None, y.map(|v| v.value)),
        PdfDestinationViewSettings::FitPageToRectangle(r) => (Some(r.left().value), Some(r.top().value)),
        _ => (None, None),
    };
    if y.is_none() && x.is_none() {
        return Some((page, None, None));
    }
    let target = document.pages().get(page as i32).ok()?;
    let tf = PageTransform::for_text(&target);
    let (px, py) = tf.point(x.unwrap_or(0.0), y.unwrap_or(0.0));
    Some((page, x.map(|_| px), y.map(|_| py)))
}

/// Encodes a link target for later resolution (after layout).
pub fn dest_link(page: usize, x: Option<f32>, y: Option<f32>) -> String {
    format!("#dest:{page}:{}:{}", x.map(|v| format!("{v:.1}")).unwrap_or_default(), y.map(|v| format!("{v:.1}")).unwrap_or_default())
}

/// Parses a link produced by [`dest_link`].
pub fn parse_dest_link(link: &str) -> Option<(usize, Option<f32>, Option<f32>)> {
    let rest = link.strip_prefix("#dest:")?;
    let mut it = rest.split(':');
    let page = it.next()?.parse().ok()?;
    let x = it.next().and_then(|v| v.parse().ok());
    let y = it.next().and_then(|v| v.parse().ok());
    Some((page, x, y))
}

/// The document outline, flattened in order.
pub fn outline(document: &PdfDocument) -> Vec<OutlineEntry> {
    fn walk(document: &PdfDocument, bm: PdfBookmark, level: usize, out: &mut Vec<OutlineEntry>, budget: &mut usize) {
        let mut cur = Some(bm);
        while let Some(b) = cur {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let target = match b.destination() {
                Some(d) => dest_target(document, &d),
                None => b.action().and_then(|a| a.as_local_destination_action().and_then(|l| l.destination().ok()).and_then(|d| dest_target(document, &d))),
            };
            if let (Some(title), Some(target)) = (b.title(), target) {
                {
                    let (page, _, y) = target;
                    let title = title.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string();
                    if !title.is_empty() {
                        out.push(OutlineEntry { title, level, page, y });
                    }
                }
            }
            if let Some(child) = b.first_child() {
                if level < 8 {
                    walk(document, child, level + 1, out, budget);
                }
            }
            cur = b.next_sibling();
        }
    }
    let mut out = Vec::new();
    let mut budget = 5000;
    if let Some(root) = document.bookmarks().root() {
        walk(document, root, 0, &mut out, &mut budget);
    }
    out
}

/// PDF date ("D:20240131123000+01'00'") to ISO 8601.
pub fn pdf_date(s: &str) -> Option<String> {
    let d: String = s.trim().trim_start_matches("D:").chars().take_while(|c| c.is_ascii_digit()).collect();
    if d.len() < 8 {
        return None;
    }
    let part = |a: usize, b: usize, def: &str| d.get(a..b).unwrap_or(def).to_string();
    Some(format!("{}-{}-{}T{}:{}:{}Z", part(0, 4, "2000"), part(4, 6, "01"), part(6, 8, "01"), part(8, 10, "00"), part(10, 12, "00"), part(12, 14, "00")))
}

fn clean(s: Option<String>) -> String {
    s.unwrap_or_default().replace('\r', "\n").trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
}

/// What the page's annotations contributed: rectangles to draw as overlays.
pub struct AnnotationResult {
    /// Graphic annotations (ink, shapes, stamps …) to rasterise as overlays.
    pub overlays: Vec<Rect>,
}

/// Reads review annotations and form fields of a page into `raw`:
/// comments, markup formatting on the glyphs, form controls (as placeholder
/// glyphs) and the areas of graphic annotations.
pub fn extract_annotations(page: &PdfPage, tf: &PageTransform, raw: &mut RawPage) -> AnnotationResult {
    let mut overlays = Vec::new();
    for annot in page.annotations().iter() {
        let ty = annot.annotation_type();
        if annot.is_hidden() {
            continue;
        }
        let Ok(bounds) = annot.bounds() else { continue };
        let bbox = tf.rect(&bounds);
        let contents = clean(annot.contents());
        let author = clean(annot.creator());
        let date = annot.modification_date().or_else(|| annot.creation_date()).and_then(|d| pdf_date(&d));
        let comment = |raw: &mut RawPage, bbox: Rect| {
            if !contents.is_empty() {
                raw.comments.push(RawComment { bbox, author: author.clone(), text: contents.clone(), date: date.clone() });
            }
        };
        match ty {
            PdfPageAnnotationType::Link | PdfPageAnnotationType::Popup | PdfPageAnnotationType::Unknown => {}
            // Sticky notes become Word comments (their icon is not drawn).
            PdfPageAnnotationType::Text => comment(raw, bbox),
            PdfPageAnnotationType::Highlight | PdfPageAnnotationType::Underline | PdfPageAnnotationType::Squiggly | PdfPageAnnotationType::Strikeout => {
                let mut quads: Vec<Rect> = annot.attachment_points().iter().map(|q| tf.rect(&q.to_rect())).collect();
                if quads.is_empty() {
                    quads.push(bbox);
                }
                // Not `stroke_color()`: for annotations with appearance streams
                // pdfium-render falls back to a page-object call that crashes PDFium.
                let color = Rgb(255, 255, 0);
                let mut new_styles = Vec::new();
                for (gi, g) in raw.glyphs.iter().enumerate() {
                    if quads.iter().any(|q| q.expand(0.5).contains_point(g.bbox.cx(), g.bbox.cy())) {
                        let mut st = raw.styles[g.style].clone();
                        match ty {
                            PdfPageAnnotationType::Highlight => st.highlight = Some(color),
                            PdfPageAnnotationType::Strikeout => st.strike = true,
                            _ => st.underline = true,
                        }
                        new_styles.push((gi, st));
                    }
                }
                for (gi, st) in new_styles {
                    let id = raw.style_id(st);
                    raw.glyphs[gi].style = id;
                }
                let area = quads.iter().skip(1).fold(quads[0], |a, q| a.union(q));
                comment(raw, area);
            }
            PdfPageAnnotationType::Widget => {
                if !add_form_field(&annot, bbox, raw) {
                    overlays.push(bbox);
                }
            }
            _ => {
                // Ink, shapes, stamps, free text …: drawn over the page.
                overlays.push(bbox);
                if ty != PdfPageAnnotationType::FreeText {
                    comment(raw, bbox);
                }
            }
        }
    }
    AnnotationResult { overlays }
}

/// Adds a form field as a placeholder glyph standing where the widget is.
fn add_form_field(annot: &PdfPageAnnotation, bbox: Rect, raw: &mut RawPage) -> bool {
    let Some(field) = annot.as_form_field() else { return false };
    let name = field.name().unwrap_or_default();
    let kind = match field.field_type() {
        PdfFormFieldType::Text => {
            let f = field.as_text_field().unwrap();
            ControlKind::Text { value: f.value().unwrap_or_default(), multiline: f.is_multiline() }
        }
        PdfFormFieldType::Checkbox => ControlKind::Check { checked: field.as_checkbox_field().and_then(|f| f.is_checked().ok()).unwrap_or(false) },
        PdfFormFieldType::RadioButton => ControlKind::Check { checked: field.as_radio_button_field().and_then(|f| f.is_checked().ok()).unwrap_or(false) },
        PdfFormFieldType::ComboBox => {
            let f = field.as_combo_box_field().unwrap();
            ControlKind::Dropdown { items: f.options().iter().filter_map(|o| o.label().cloned()).collect(), value: f.value().unwrap_or_default() }
        }
        PdfFormFieldType::ListBox => {
            let f = field.as_list_box_field().unwrap();
            ControlKind::Dropdown { items: f.options().iter().filter_map(|o| o.label().cloned()).collect(), value: f.value().unwrap_or_default() }
        }
        _ => return false,
    };
    if bbox.width() < 2.0 || bbox.height() < 2.0 {
        return false;
    }
    let index = raw.fields.len();
    let Some(ch) = char::from_u32(CONTROL_BASE + index as u32) else { return false };
    raw.fields.push(FormControl { name: name.trim_matches('\0').to_string(), kind, width: bbox.width() });
    // The placeholder sits on a text line of a sensible size inside the widget.
    let size = (bbox.height() * 0.7).clamp(6.0, 12.0);
    let style = TextStyle { font: "Arial".into(), size, generic: Generic::Sans, ..Default::default() };
    let style = raw.style_id(style);
    let baseline = (bbox.cy() + 0.35 * size).min(bbox.y1);
    raw.glyphs.push(Glyph {
        ch,
        bbox: Rect::new(bbox.x0, baseline - 0.8 * size, bbox.x1, baseline + 0.2 * size),
        baseline,
        style,
        space_before: true,
        link: None,
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_links() {
        assert_eq!(pdf_date("D:20240131123005+01'00'").as_deref(), Some("2024-01-31T12:30:05Z"));
        assert_eq!(pdf_date("D:2024").as_deref(), None);
        let l = dest_link(3, None, Some(120.0));
        assert_eq!(parse_dest_link(&l), Some((3, None, Some(120.0))));
    }
}
