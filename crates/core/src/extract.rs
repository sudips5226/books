//! Extraction of positioned glyphs, images, ruling lines and filled boxes from a
//! PDF page using PDFium.

use std::collections::HashMap;
use std::io::Cursor;

use image::{DynamicImage, GenericImageView, ImageFormat};
use pdfium_render::prelude::*;

use crate::fonts::{analyse_font_name, generic_family};
use crate::model::*;

/// Maps PDF user-space coordinates (bottom-left origin, possibly rotated) to
/// displayed page coordinates (top-left origin).
#[derive(Clone, Copy, Debug)]
pub struct PageTransform {
    rot: u8,
    /// The page's own /Rotate (quarter turns); differs from `rot` when ignored.
    page_rot: u8,
    ox: f32,
    oy: f32,
    w0: f32,
    h0: f32,
}

impl PageTransform {
    /// The transform that shows most of the page's text upright.
    ///
    /// `/Rotate` is used two ways: landscape pages whose content is drawn
    /// rotated so that it displays upright (keep the rotation), and ordinary
    /// pages that are merely displayed turned (ignore it, so the text stays
    /// readable and editable).
    pub fn for_text(page: &PdfPage) -> Self {
        let tf = Self::new(page);
        if tf.rot == 0 {
            return tf;
        }
        let (mut display_upright, mut user_upright) = (0usize, 0usize);
        if let Ok(text) = page.text() {
            for ch in text.chars().iter().take(400) {
                if ch.unicode_char().is_none_or(|c| c.is_whitespace()) {
                    continue;
                }
                let a = ch.angle_degrees().unwrap_or(0.0).rem_euclid(360.0);
                let near = |t: f32| {
                    let d = (a - t).rem_euclid(360.0);
                    d.min(360.0 - d) < 4.0
                };
                if near(0.0) {
                    user_upright += 1;
                } else if near(tf.rotation_degrees()) || near(360.0 - tf.rotation_degrees()) {
                    display_upright += 1;
                }
            }
        }
        if user_upright > display_upright {
            Self { rot: 0, ..tf }
        } else {
            tf
        }
    }

    pub fn new(page: &PdfPage) -> Self {
        let crop = page
            .boundaries()
            .crop()
            .or_else(|_| page.boundaries().media())
            .map(|b| b.bounds)
            .unwrap_or_else(|_| PdfRect::new_from_values(0.0, 0.0, page.height().value, page.width().value));
        let rot = match page.rotation() {
            Ok(PdfPageRenderRotation::Degrees90) => 1,
            Ok(PdfPageRenderRotation::Degrees180) => 2,
            Ok(PdfPageRenderRotation::Degrees270) => 3,
            _ => 0,
        };
        Self {
            rot,
            page_rot: rot,
            ox: crop.left().value,
            oy: crop.bottom().value,
            w0: crop.width().value,
            h0: crop.height().value,
        }
    }

    pub fn width(&self) -> f32 {
        if self.rot % 2 == 1 { self.h0 } else { self.w0 }
    }

    pub fn height(&self) -> f32 {
        if self.rot % 2 == 1 { self.w0 } else { self.h0 }
    }

    /// Rotation of the page in degrees (clockwise).
    pub fn rotation_degrees(&self) -> f32 {
        self.rot as f32 * 90.0
    }

    /// Renders the page content (without annotations and form fields, which are
    /// converted separately) in this transform's orientation at `scale` pixels per point.
    pub fn render(&self, page: &PdfPage, scale: f32) -> anyhow::Result<DynamicImage> {
        self.render_with(page, scale, false)
    }

    /// Like [`PageTransform::render`], optionally with annotations and form fields.
    pub fn render_with(&self, page: &PdfPage, scale: f32, annotations: bool) -> anyhow::Result<DynamicImage> {
        // PDFium always applies /Rotate: render as displayed, then turn the
        // bitmap back by the part of the rotation this transform ignores.
        let display_w = if self.page_rot % 2 == 1 { self.h0 } else { self.w0 };
        let bitmap = page.render_with_config(
            &PdfRenderConfig::new()
                .set_target_width((display_w * scale).round() as i32)
                .set_maximum_height(20000)
                .render_form_data(annotations)
                .render_annotations(annotations)
                .set_clear_color(PdfColor::WHITE),
        )?;
        let img = bitmap.as_image()?;
        Ok(match (self.page_rot + 4 - self.rot) % 4 {
            1 => img.rotate270(),
            2 => img.rotate180(),
            3 => img.rotate90(),
            _ => img,
        })
    }

    pub fn point(&self, x: f32, y: f32) -> (f32, f32) {
        let x = x - self.ox;
        let y = y - self.oy;
        match self.rot {
            1 => (y, x),
            2 => (self.w0 - x, y),
            3 => (self.h0 - y, self.w0 - x),
            _ => (x, self.h0 - y),
        }
    }

    pub fn rect(&self, r: &PdfRect) -> Rect {
        let (ax, ay) = self.point(r.left().value, r.bottom().value);
        let (bx, by) = self.point(r.right().value, r.top().value);
        Rect::from_points(ax, ay, bx, by)
    }
}

/// Options controlling extraction.
#[derive(Clone, Debug)]
pub struct ExtractOptions {
    pub extract_images: bool,
    /// Render clusters of vector graphics (charts, diagrams) to images.
    pub rasterize_vector_graphics: bool,
    /// DPI used when rasterising vector graphics.
    pub graphics_dpi: f32,
    /// Maximum pixel size of the longest edge of an embedded image.
    pub max_image_px: u32,
    /// Rebuild simple vector figures as editable shapes (the rendering stays as fallback).
    pub vector_shapes: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            extract_images: true,
            rasterize_vector_graphics: true,
            graphics_dpi: 150.0,
            max_image_px: 2400,
            vector_shapes: true,
        }
    }
}

/// Information that is useful for deciding whether a page needs OCR.
#[derive(Clone, Debug, Default)]
pub struct PageStats {
    pub visible_chars: usize,
    pub invisible_chars: usize,
    pub garbage_chars: usize,
    /// Fraction of the page area covered by raster images.
    pub image_coverage: f32,
}

impl PageStats {
    /// True if the page looks like a scan without a usable text layer.
    pub fn looks_scanned(&self) -> bool {
        let usable = self.visible_chars + self.invisible_chars;
        let garbage = usable > 0 && self.garbage_chars as f32 / usable as f32 > 0.4;
        (usable < 16 && self.image_coverage > 0.25) || garbage
    }
}

struct PathInfo {
    bbox: Rect,
    stroked: bool,
    filled: bool,
    fill: Rgb,
    stroke: Rgb,
    stroke_width: f32,
    segments: usize,
    has_curves: bool,
    /// Geometry in page coordinates (simple paths only).
    cmds: Option<Vec<PathCmd>>,
    /// Not a path: a shading or other content shapes cannot express.
    opaque: bool,
}

/// Path segments in page coordinates; Béziers come as three consecutive points.
fn path_cmds(segs: &PdfPagePathObjectSegments, tf: &PageTransform) -> Option<Vec<PathCmd>> {
    let mut out = Vec::new();
    let mut bez: Vec<(f32, f32)> = Vec::new();
    for s in segs.iter() {
        let (x, y) = s.point();
        let p = tf.point(x.value, y.value);
        if !(p.0.is_finite() && p.1.is_finite()) {
            return None;
        }
        match s.segment_type() {
            PdfPathSegmentType::MoveTo => out.push(PathCmd::MoveTo(p.0, p.1)),
            PdfPathSegmentType::LineTo => out.push(PathCmd::LineTo(p.0, p.1)),
            PdfPathSegmentType::BezierTo => {
                bez.push(p);
                if bez.len() == 3 {
                    out.push(PathCmd::CurveTo(bez[0].0, bez[0].1, bez[1].0, bez[1].1, bez[2].0, bez[2].1));
                    bez.clear();
                }
            }
            _ => return None,
        }
        if s.is_close() {
            out.push(PathCmd::Close);
        }
    }
    (!out.is_empty() && bez.is_empty()).then_some(out)
}

fn pdf_color(c: Result<PdfColor, PdfiumError>) -> Option<(Rgb, u8)> {
    c.ok().map(|c| (Rgb(c.red(), c.green(), c.blue()), c.alpha()))
}

fn is_garbage(ch: char) -> bool {
    ch == '\u{FFFD}' || ('\u{E000}'..='\u{F8FF}').contains(&ch) || (ch.is_control() && !ch.is_whitespace())
}

/// Extracts the content of a page.
pub fn extract_page(
    document: &PdfDocument,
    page: &PdfPage,
    index: usize,
    opts: &ExtractOptions,
) -> anyhow::Result<(RawPage, PageStats)> {
    let tf = PageTransform::for_text(page);
    let mut raw = RawPage {
        index,
        width: tf.width(),
        height: tf.height(),
        ..Default::default()
    };
    let mut stats = PageStats::default();
    let page_rect = Rect::new(0.0, 0.0, raw.width, raw.height);

    // ---------------------------------------------------------------- links
    let mut link_rects: Vec<(Rect, usize)> = Vec::new();
    for link in page.links().iter() {
        let Ok(r) = link.rect() else { continue };
        let action = link.action();
        let uri = action.as_ref().and_then(|a| a.as_uri_action().and_then(|u| u.uri().ok()));
        let target = match uri {
            Some(uri) => uri.trim_end_matches('\0').trim().to_string(),
            // Jumps inside the document are resolved to bookmarks after layout.
            None => {
                let dest = match link.destination() {
                    Some(d) => crate::annots::dest_target(document, &d),
                    None => action.as_ref().and_then(|a| a.as_local_destination_action()).and_then(|l| l.destination().ok()).and_then(|d| crate::annots::dest_target(document, &d)),
                };
                dest.map(|(p, x, y)| crate::annots::dest_link(p, x, y)).unwrap_or_default()
            }
        };
        if target.is_empty() {
            continue;
        }
        raw.links.push(target);
        link_rects.push((tf.rect(&r), raw.links.len() - 1));
    }

    // ---------------------------------------------------------------- glyphs
    let mut graphic_hints: Vec<Rect> = Vec::new();
    let mut invisible: Vec<Glyph> = Vec::new();
    {
        let text = page.text()?;
        let mut pending_space = false;
        let mut style_cache: HashMap<(String, u32, u8, Rgb), usize> = HashMap::new();
        // Horizontal text on a rotated page is rotated in user space; accept either direction.
        let expected_angles = [tf.rotation_degrees() % 360.0, (360.0 - tf.rotation_degrees()) % 360.0];

        for ch in text.chars().iter() {
            let Some(c) = ch.unicode_char() else { continue };
            if c.is_whitespace() || c == '\u{0}' {
                pending_space = true;
                continue;
            }
            // PDFium reports end-of-line hyphenation hyphens as U+0002.
            let c = if c == '\u{2}' { '\u{AD}' } else { c };
            let font_name = ch.font_name();
            // TeX's extension font stores big operators at ASCII positions.
            let c = if font_name.to_ascii_lowercase().contains("cmex") { crate::math::cmex_char(c) } else { c };
            if c == '\u{3}' {
                continue;
            }
            let Ok(loose) = ch.loose_bounds() else { continue };
            let tight = ch.tight_bounds().ok();
            let mut size = ch.scaled_font_size().value;
            let mut bbox = tf.rect(&loose);
            if let Some(t) = tight {
                let tb = tf.rect(&t);
                // Some fonts carry absurd ascent/descent values; fall back to the ink box.
                if bbox.height() > 3.0 * size.max(tb.height()) || bbox.is_empty() {
                    bbox = Rect::new(tb.x0, tb.y1 - size * 0.8, tb.x1, tb.y1 + size * 0.2);
                }
            }
            if !(size.is_finite()) || size <= 0.5 || size > 500.0 {
                size = bbox.height().max(1.0) / 1.15;
            }

            let angle = ch.angle_degrees().unwrap_or(0.0).rem_euclid(360.0);
            let angle_diff = expected_angles
                .iter()
                .map(|e| {
                    let d = (angle - e).abs() % 360.0;
                    d.min(360.0 - d)
                })
                .fold(f32::MAX, f32::min);
            // Rotated text becomes a text box (or part of a figure it belongs to).
            let rotated = angle_diff > 4.0;
            if rotated {
                // The scaled size is not meaningful for rotated text: recover the
                // line height H from the axis-aligned box (w = L·c + H·s, h = L·s + H·c).
                let t = angle.to_radians();
                let (c, s) = (t.cos().abs(), t.sin().abs());
                let (w, h) = (bbox.width(), bbox.height());
                let height = if (c * c - s * s).abs() > 0.2 { (h * c - w * s) / (c * c - s * s) } else { ch.unscaled_font_size().value * 1.15 };
                if height.is_finite() && height > 1.0 {
                    size = height / 1.15;
                }
            }
            // PDFium measures the angle clockwise; convert to counter-clockwise on the displayed page.
            let display_angle = (360.0 - angle - tf.rotation_degrees()).rem_euclid(360.0);

            if is_garbage(c) {
                stats.garbage_chars += 1;
            }

            let baseline = ch
                .origin()
                .map(|(x, y)| tf.point(x.value, y.value).1)
                .unwrap_or(bbox.y1 - size * 0.2);
            let baseline = if (baseline - bbox.y1).abs() > size || baseline < bbox.y0 {
                bbox.y1 - (bbox.height() * 0.2)
            } else {
                baseline
            };

            let render_mode = ch.render_mode().ok();
            let is_invisible = matches!(render_mode, Some(PdfPageTextRenderMode::Invisible));

            let (color, alpha) = pdf_color(ch.fill_color()).unwrap_or((Rgb::BLACK, 255));
            let color = if is_invisible || alpha < 20 { Rgb::BLACK } else { color.snapped() };

            let weight_bold = matches!(
                ch.font_weight(),
                Some(PdfFontWeight::Weight600)
                    | Some(PdfFontWeight::Weight700Bold)
                    | Some(PdfFontWeight::Weight800)
                    | Some(PdfFontWeight::Weight900)
            ) || matches!(ch.font_weight(), Some(PdfFontWeight::Custom(w)) if w >= 600);
            let flags: u8 = (weight_bold as u8)
                | ((ch.font_is_italic() as u8) << 1)
                | ((ch.font_is_fixed_pitch() as u8) << 2)
                | ((ch.font_is_serif() as u8) << 3)
                | ((ch.font_is_small_caps() as u8) << 4)
                | ((ch.font_is_bold_reenforced() as u8) << 5);
            let size_key = (size * 4.0).round() as u32;
            let key = (font_name.clone(), size_key, flags, color);

            let style = match style_cache.get(&key) {
                Some(&s) => s,
                None => {
                    let info = analyse_font_name(&font_name);
                    let family = if font_name.trim().is_empty() {
                        generic_family(flags & 8 != 0, flags & 4 != 0).to_string()
                    } else {
                        info.family
                    };
                    let math = crate::math::is_math_font(&font_name);
                    // Maths fonts map to Word's maths font.
                    let family = if math { "Cambria Math".to_string() } else { family };
                    let generic = match crate::fonts::generic_of(&family) {
                        Generic::Unknown if flags & 4 != 0 => Generic::Mono,
                        Generic::Unknown if flags & 8 != 0 => Generic::Serif,
                        Generic::Unknown => Generic::Sans,
                        g => g,
                    };
                    let st = TextStyle {
                        generic,
                        math,
                        font: family,
                        size: (size * 2.0).round() / 2.0,
                        bold: info.bold || flags & 1 != 0 || flags & 32 != 0,
                        italic: info.italic || flags & 2 != 0,
                        small_caps: flags & 16 != 0,
                        color,
                        ..Default::default()
                    };
                    let id = raw.style_id(st);
                    style_cache.insert(key, id);
                    id
                }
            };

            let link = link_rects
                .iter()
                .find(|(r, _)| r.expand(1.0).contains_point(bbox.cx(), bbox.cy()))
                .map(|(_, i)| *i);

            let glyph = Glyph {
                ch: c,
                bbox,
                baseline,
                style,
                space_before: pending_space,
                link,
            };
            pending_space = false;
            if rotated {
                raw.rotated.push((glyph, display_angle));
            } else if is_invisible {
                stats.invisible_chars += 1;
                invisible.push(glyph);
            } else {
                stats.visible_chars += 1;
                raw.glyphs.push(glyph);
            }
        }
    }
    // Pages whose only text is invisible (OCR layer of a scanned PDF) use that text.
    if raw.glyphs.len() < invisible.len() {
        raw.glyphs.extend(invisible);
    }
    dedupe_fake_bold(&mut raw);

    // ---------------------------------------------------------------- annotations
    let annotations = crate::annots::extract_annotations(page, &tf, &mut raw);

    // ---------------------------------------------------------------- objects
    let mut paths: Vec<PathInfo> = Vec::new();
    let mut image_area = 0.0f32;
    for object in page.objects().iter() {
        collect_object(document, &object, &tf, opts, &mut raw, &mut paths, &mut image_area, None, 0);
    }
    stats.image_coverage = (image_area / page_rect.area().max(1.0)).min(1.0);

    classify_paths(&mut raw, &paths, &mut graphic_hints, &page_rect);

    // Content outside the crop box is invisible: drop it, clip the rest.
    let visible = page_rect.expand(0.5);
    raw.glyphs.retain(|g| visible.contains_point(g.bbox.cx(), g.bbox.cy()));
    raw.rotated.retain(|(g, _)| visible.contains_point(g.bbox.cx(), g.bbox.cy()));
    raw.images.retain_mut(|im| {
        let clipped = im.bbox.intersection(&page_rect);
        let keep = !clipped.is_empty() && clipped.area() > 0.2 * im.bbox.area().max(1.0);
        im.bbox = clipped;
        keep
    });
    raw.rules.retain_mut(|r| {
        let clipped = r.bbox.intersection(&visible);
        let keep = clipped.width() > 0.0 && clipped.height() > 0.0;
        r.bbox = clipped;
        keep
    });
    raw.boxes.retain_mut(|b| {
        let clipped = b.bbox.intersection(&page_rect);
        let keep = !clipped.is_empty();
        b.bbox = clipped;
        keep
    });

    // Equations before figure detection: fraction bars are not chart axes.
    let (equations, complex_math) = crate::math::extract_display_equations(&mut raw);
    raw.equations = equations;

    let mut regions: Vec<(Rect, f32)> = Vec::new();
    if opts.rasterize_vector_graphics && (!graphic_hints.is_empty() || !raw.boxes.is_empty()) {
        regions = cluster_graphics(&raw, &graphic_hints).into_iter().map(|r| (r, 1.5)).collect();
    }
    // Matrices and other structures the equation builder cannot express are kept
    // as pictures (glyph boxes already include their full height: no padding).
    if opts.extract_images {
        regions.extend(complex_math.into_iter().map(|r| (r, 0.0)));
    }
    if !regions.is_empty() {
        if let Err(e) = rasterize_regions(page, &tf, &mut raw, &regions, &paths, opts) {
            log::warn!("page {}: could not rasterise vector graphics: {e}", index + 1);
        }
    }
    if opts.extract_images && !annotations.overlays.is_empty() {
        if let Err(e) = render_overlays(page, &tf, &mut raw, &annotations.overlays, opts) {
            log::warn!("page {}: could not render annotations: {e}", index + 1);
        }
    }

    Ok((raw, stats))
}

#[allow(clippy::too_many_arguments)]
fn collect_object(
    document: &PdfDocument,
    object: &PdfPageObject,
    tf: &PageTransform,
    opts: &ExtractOptions,
    raw: &mut RawPage,
    paths: &mut Vec<PathInfo>,
    image_area: &mut f32,
    form_matrix: Option<PdfMatrix>,
    depth: usize,
) {
    let bounds = match object.bounds() {
        Ok(b) => {
            let r = b.to_rect();
            match form_matrix {
                Some(m) => r.transform(m),
                None => r,
            }
        }
        Err(_) => return,
    };
    let bbox = tf.rect(&bounds);
    match object {
        PdfPageObject::Image(img) => {
            *image_area += bbox.intersection(&Rect::new(0.0, 0.0, raw.width, raw.height)).area().max(0.0);
            if !opts.extract_images || bbox.width() < 3.0 || bbox.height() < 3.0 {
                return;
            }
            let decoded = img
                .get_processed_image(document)
                .or_else(|_| img.get_raw_image());
            match decoded {
                Ok(di) => {
                    if let Some(data) = encode_image(&di, opts.max_image_px) {
                        raw.images.push(RawImage { bbox, data, behind_text: false, overlay: false, vector: None, alt: None });
                    }
                }
                Err(e) => log::debug!("skipping undecodable image: {e}"),
            }
        }
        PdfPageObject::Path(path) => {
            let fill = pdf_color(object.fill_color());
            let stroke = pdf_color(object.stroke_color());
            let fill_mode = path.fill_mode().unwrap_or(PdfPathFillMode::None);
            let filled = !matches!(fill_mode, PdfPathFillMode::None) && fill.is_some_and(|(_, a)| a > 10);
            let stroked = path.is_stroked().unwrap_or(false) && stroke.is_some_and(|(_, a)| a > 10);
            let segs = path.segments();
            let mut has_curves = false;
            let n = segs.len() as usize;
            if n <= 64 {
                for s in segs.iter() {
                    if s.segment_type() == PdfPathSegmentType::BezierTo {
                        has_curves = true;
                        break;
                    }
                }
            } else {
                has_curves = true;
            }
            // Geometry for editable shapes: the path's matrix (inside forms, combined with theirs).
            let matrix = path.matrix().ok().map(|own| match form_matrix {
                Some(parent) => own.multiply(parent),
                None => own,
            });
            let scale = matrix.map(|m| (m.a() * m.d() - m.b() * m.c()).abs().sqrt()).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
            let cmds = if opts.vector_shapes && n <= 400 { matrix.and_then(|m| path_cmds(&segs.transform(m), tf)) } else { None };
            paths.push(PathInfo {
                bbox,
                stroked,
                filled,
                fill: fill.map(|f| f.0).unwrap_or(Rgb::BLACK),
                stroke: stroke.map(|s| s.0).unwrap_or(Rgb::BLACK),
                stroke_width: object.stroke_width().map(|w| w.value).unwrap_or(1.0) * scale,
                segments: n,
                has_curves,
                cmds,
                opaque: false,
            });
        }
        PdfPageObject::XObjectForm(form) => {
            if depth > 4 {
                return;
            }
            let m = form.matrix().ok();
            let combined = match (form_matrix, m) {
                (Some(parent), Some(own)) => Some(own.multiply(parent)),
                (None, Some(own)) => Some(own),
                (p, None) => p,
            };
            for child in form.iter() {
                collect_object(document, &child, tf, opts, raw, paths, image_area, combined, depth + 1);
            }
        }
        PdfPageObject::Shading(_) => {
            paths.push(PathInfo { bbox, stroked: false, filled: false, fill: Rgb::WHITE, stroke: Rgb::WHITE, stroke_width: 0.0, segments: 0, has_curves: false, cmds: None, opaque: true });
            raw.boxes.push(FilledBox { bbox, color: Rgb(230, 230, 230) });
        }
        _ => {}
    }
}

/// Encodes an image as PNG (or JPEG for large opaque photos), downscaling if needed.
pub fn encode_image(img: &DynamicImage, max_px: u32) -> Option<ImageData> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let img = if w.max(h) > max_px {
        img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
    } else {
        img.clone()
    };
    let (w, h) = img.dimensions();
    let has_alpha = img.color().has_alpha() && {
        let rgba = img.to_rgba8();
        rgba.pixels().any(|p| p.0[3] < 250)
    };
    let mut buf = Cursor::new(Vec::new());
    let photo = !has_alpha && (w as u64 * h as u64) > 250_000 && img.color().channel_count() >= 3;
    let ext = if photo {
        let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 88);
        rgb.write_with_encoder(enc).ok()?;
        "jpeg"
    } else {
        let img = if has_alpha { DynamicImage::ImageRgba8(img.to_rgba8()) } else { DynamicImage::ImageRgb8(img.to_rgb8()) };
        img.write_to(&mut buf, ImageFormat::Png).ok()?;
        "png"
    };
    Some(ImageData { bytes: buf.into_inner(), ext, px_width: w, px_height: h })
}

/// Text drawn twice with a tiny offset ("fake bold") is collapsed into a single bold glyph.
fn dedupe_fake_bold(raw: &mut RawPage) {
    if raw.glyphs.len() < 2 {
        return;
    }
    let mut buckets: HashMap<(char, i32), Vec<usize>> = HashMap::new();
    let mut remove = vec![false; raw.glyphs.len()];
    let mut make_bold: Vec<usize> = Vec::new();
    for i in 0..raw.glyphs.len() {
        let g = &raw.glyphs[i];
        let key = (g.ch, (g.baseline / 2.0).round() as i32);
        let size = raw.styles[g.style].size;
        let mut dup_of = None;
        for k in [key, (key.0, key.1 - 1), (key.0, key.1 + 1)] {
            if let Some(list) = buckets.get(&k) {
                for &j in list {
                    let o = &raw.glyphs[j];
                    if (o.bbox.x0 - g.bbox.x0).abs() < 0.15 * size && (o.baseline - g.baseline).abs() < 0.15 * size {
                        dup_of = Some(j);
                        break;
                    }
                }
            }
            if dup_of.is_some() {
                break;
            }
        }
        match dup_of {
            Some(j) => {
                remove[i] = true;
                make_bold.push(j);
            }
            None => buckets.entry(key).or_default().push(i),
        }
    }
    if make_bold.is_empty() {
        return;
    }
    for j in make_bold {
        let mut st = raw.styles[raw.glyphs[j].style].clone();
        if !st.bold {
            st.bold = true;
            raw.glyphs[j].style = raw.style_id(st);
        }
    }
    let mut idx = 0;
    raw.glyphs.retain(|_| {
        let keep = !remove[idx];
        idx += 1;
        keep
    });
}

/// Turns path objects into rules, filled boxes or graphic hints.
fn classify_paths(raw: &mut RawPage, paths: &[PathInfo], hints: &mut Vec<Rect>, page: &Rect) {
    for p in paths.iter().filter(|p| !p.opaque) {
        let b = p.bbox;
        if b.width() <= 0.0 && b.height() <= 0.0 {
            continue;
        }
        // Clip to the page.
        if !b.intersects(page) {
            continue;
        }
        let color = if p.stroked { p.stroke } else { p.fill };
        let thin_h = b.height() <= 2.5 && b.width() >= 3.0;
        let thin_v = b.width() <= 2.5 && b.height() >= 3.0;
        if (thin_h || thin_v) && !p.has_curves && (p.stroked || p.filled) {
            if color.is_near_white() {
                continue;
            }
            let (orientation, width) = if thin_h {
                (RuleOrientation::Horizontal, if p.stroked { p.stroke_width.max(b.height()) } else { b.height() })
            } else {
                (RuleOrientation::Vertical, if p.stroked { p.stroke_width.max(b.width()) } else { b.width() })
            };
            raw.rules.push(Rule { bbox: b, orientation, color, width: width.clamp(0.25, 6.0) });
            continue;
        }
        let rect_like = !p.has_curves && (4..=6).contains(&p.segments);
        if rect_like {
            if b.area() > page.area() * 0.85 {
                // Page background colour.
                if p.filled && p.fill != Rgb::WHITE && p.fill.luminance() < 254.0 {
                    raw.background = Some(p.fill);
                }
                continue;
            }
            if p.filled && !p.fill.is_near_white() {
                raw.boxes.push(FilledBox { bbox: b, color: p.fill });
            }
            if p.stroked && !p.stroke.is_near_white() {
                let w = p.stroke_width.clamp(0.25, 6.0);
                let hw = w / 2.0;
                let edges = [
                    (Rect::new(b.x0, b.y0 - hw, b.x1, b.y0 + hw), RuleOrientation::Horizontal),
                    (Rect::new(b.x0, b.y1 - hw, b.x1, b.y1 + hw), RuleOrientation::Horizontal),
                    (Rect::new(b.x0 - hw, b.y0, b.x0 + hw, b.y1), RuleOrientation::Vertical),
                    (Rect::new(b.x1 - hw, b.y0, b.x1 + hw, b.y1), RuleOrientation::Vertical),
                ];
                for (r, o) in edges {
                    raw.rules.push(Rule { bbox: r, orientation: o, color: p.stroke, width: w });
                }
            }
            continue;
        }
        if b.area() > page.area() * 0.85 {
            continue;
        }
        if (p.filled && !p.fill.is_near_white()) || (p.stroked && !p.stroke.is_near_white()) {
            hints.push(b);
        }
    }
}

/// Groups graphic hints (complex paths, rotated text, empty filled boxes) into
/// figure regions worth rasterising.
fn cluster_graphics(raw: &RawPage, hints: &[Rect]) -> Vec<Rect> {
    let mut items: Vec<Rect> = hints.to_vec();
    // Text-free filled boxes (bars of a chart) take part in clustering as well.
    for b in &raw.boxes {
        let has_text = raw.glyphs.iter().any(|g| b.bbox.contains_point(g.bbox.cx(), g.bbox.cy()));
        if !has_text {
            items.push(b.bbox);
        }
    }
    let n_hints = hints.len();
    let n_scored = items.len();
    if n_scored == 0 {
        return vec![];
    }
    // Rules connect the parts of a chart (axes, grid lines) but do not make a figure on their own.
    items.extend(raw.rules.iter().map(|r| r.bbox));
    // Union-find on proximity.
    let mut parent: Vec<usize> = (0..items.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut i = i;
        while p[i] != r {
            let n = p[i];
            p[i] = r;
            i = n;
        }
        r
    }
    for i in 0..items.len() {
        for j in i + 1..items.len() {
            if items[i].expand(4.0).intersects(&items[j]) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut groups: HashMap<usize, (Rect, usize, usize)> = HashMap::new();
    for i in 0..items.len() {
        let r = find(&mut parent, i);
        let e = groups.entry(r).or_insert((items[i], 0, 0));
        e.0 = e.0.union(&items[i]);
        if i < n_hints {
            e.1 += 1;
        } else if i < n_scored {
            e.2 += 1;
        }
    }
    let page_area = raw.width * raw.height;
    let runs = text_runs(raw);
    let mut out = Vec::new();
    for (_, (mut rect, n_complex, n_boxes)) in groups {
        if rect.area() < 600.0 || rect.width() < 12.0 || rect.height() < 12.0 {
            continue;
        }
        if rect.area() > page_area * 0.8 {
            continue;
        }
        if n_complex == 0 && n_boxes < 3 {
            continue;
        }
        // Pull in axes, tick marks, grid lines and short labels around the graphic.
        for _ in 0..3 {
            let grown = rect.expand(4.0);
            for r in &raw.rules {
                if grown.intersects(&r.bbox) && r.bbox.width().max(r.bbox.height()) < 2.0 * rect.width().max(rect.height()) {
                    rect = rect.union(&r.bbox);
                }
            }
            let near = rect.expand(12.0);
            for run in &runs {
                if run.1 <= 20 && near.intersects(&run.0) {
                    rect = rect.union(&run.0);
                }
            }
            // Rotated axis titles next to a chart belong to it.
            for (g, _) in &raw.rotated {
                if near.intersects(&g.bbox) {
                    rect = rect.union(&g.bbox);
                }
            }
        }
        // Do not swallow regular text: if text density is high, it's a decorated text box.
        let text_area: f32 = raw
            .glyphs
            .iter()
            .filter(|g| rect.contains_point(g.bbox.cx(), g.bbox.cy()))
            .map(|g| g.bbox.area())
            .sum();
        if text_area / rect.area() > 0.22 {
            continue;
        }
        out.push(rect);
    }
    // Merge overlapping regions.
    let mut merged: Vec<Rect> = Vec::new();
    for r in out {
        if let Some(m) = merged.iter_mut().find(|m| m.intersects(&r)) {
            *m = m.union(&r);
        } else {
            merged.push(r);
        }
    }
    merged
}

/// Groups glyphs into short horizontal runs (words / labels): (bbox, char count).
fn text_runs(raw: &RawPage) -> Vec<(Rect, usize)> {
    let mut runs: Vec<(Rect, usize)> = Vec::new();
    let mut last: Option<&Glyph> = None;
    for g in &raw.glyphs {
        let size = raw.styles[g.style].size;
        let cont = last.is_some_and(|l| {
            (l.baseline - g.baseline).abs() < 0.3 * size && g.bbox.x0 - l.bbox.x1 < 0.6 * size && g.bbox.x0 >= l.bbox.x0
        });
        match runs.last_mut() {
            Some(r) if cont => {
                r.0 = r.0.union(&g.bbox);
                r.1 += 1;
            }
            _ => runs.push((g.bbox, 1)),
        }
        last = Some(g);
    }
    runs
}

/// Renders figure regions to images, removing the glyphs/images/rules they cover.
/// Rebuilds a figure region from its paths and labels, if it is simple enough.
fn vector_drawing(raw: &RawPage, paths: &[PathInfo], r: &Rect) -> Option<VectorDrawing> {
    let inside = |b: &Rect| b.intersects(r) && (b.fraction_inside(&r.expand(1.0)) > 0.95 || b.area() == 0.0 && r.expand(1.0).contains_point(b.cx(), b.cy()));
    // Pictures, shadings and rotated text cannot be expressed as shapes.
    if raw.images.iter().any(|im| im.bbox.intersects(r))
        || paths.iter().any(|p| p.opaque && p.bbox.intersects(r))
        || raw.rotated.iter().any(|(g, _)| r.contains_point(g.bbox.cx(), g.bbox.cy()))
    {
        return None;
    }
    let mut shapes = Vec::new();
    let mut total = 0;
    for p in paths.iter().filter(|p| !p.opaque && p.bbox.intersects(r)) {
        if !inside(&p.bbox) {
            return None; // cut off by the region (or clipped): keep the picture
        }
        if !p.filled && !p.stroked {
            continue;
        }
        let cmds = p.cmds.as_ref()?;
        total += cmds.len();
        let rel: Vec<PathCmd> = cmds
            .iter()
            .map(|c| match *c {
                PathCmd::MoveTo(x, y) => PathCmd::MoveTo(x - r.x0, y - r.y0),
                PathCmd::LineTo(x, y) => PathCmd::LineTo(x - r.x0, y - r.y0),
                PathCmd::CurveTo(a, b, c2, d, e, f) => PathCmd::CurveTo(a - r.x0, b - r.y0, c2 - r.x0, d - r.y0, e - r.x0, f - r.y0),
                PathCmd::Close => PathCmd::Close,
            })
            .collect();
        let bbox = Rect::new(p.bbox.x0 - r.x0, p.bbox.y0 - r.y0, p.bbox.x1 - r.x0, p.bbox.y1 - r.y0);
        shapes.push(VectorShape {
            bbox,
            cmds: rel,
            fill: p.filled.then_some(p.fill),
            stroke: p.stroked.then_some((p.stroke, p.stroke_width.clamp(0.25, 20.0))),
        });
    }
    if shapes.is_empty() || shapes.len() > 300 || total > 6000 {
        return None;
    }
    // Labels: horizontal runs of glyphs inside the region.
    let mut labels: Vec<VectorLabel> = Vec::new();
    let mut glyphs: Vec<&Glyph> = raw.glyphs.iter().filter(|g| r.contains_point(g.bbox.cx(), g.bbox.cy())).collect();
    glyphs.sort_by_key(|g| (g.baseline.round() as i64, g.bbox.x0 as i64));
    let mut last: Option<&Glyph> = None;
    for g in glyphs {
        let st = &raw.styles[g.style];
        let joins = last.is_some_and(|l| (l.baseline - g.baseline).abs() < 0.3 * st.size && g.bbox.x0 - l.bbox.x1 < 0.8 * st.size && l.style == g.style);
        if joins {
            let lab = labels.last_mut().unwrap();
            if g.space_before || g.bbox.x0 - last.unwrap().bbox.x1 > 0.2 * st.size {
                lab.text.push(' ');
            }
            lab.text.push(g.ch);
            lab.bbox = lab.bbox.union(&Rect::new(g.bbox.x0 - r.x0, g.bbox.y0 - r.y0, g.bbox.x1 - r.x0, g.bbox.y1 - r.y0));
        } else {
            labels.push(VectorLabel { bbox: Rect::new(g.bbox.x0 - r.x0, g.bbox.y0 - r.y0, g.bbox.x1 - r.x0, g.bbox.y1 - r.y0), text: g.ch.to_string(), style: st.clone() });
        }
        last = Some(g);
    }
    if labels.len() > 80 {
        return None;
    }
    Some(VectorDrawing { width: r.width(), height: r.height(), shapes, labels })
}

fn rasterize_regions(page: &PdfPage, tf: &PageTransform, raw: &mut RawPage, regions: &[(Rect, f32)], paths: &[PathInfo], opts: &ExtractOptions) -> anyhow::Result<()> {
    let full = tf.render(page, opts.graphics_dpi / 72.0)?;
    let (pw, ph) = full.dimensions();
    let sx = pw as f32 / raw.width;
    let sy = ph as f32 / raw.height;
    for (r, pad) in regions {
        let r = r.expand(*pad).intersection(&Rect::new(0.0, 0.0, raw.width, raw.height));
        let x = (r.x0 * sx).floor().max(0.0) as u32;
        let y = (r.y0 * sy).floor().max(0.0) as u32;
        let w = ((r.width() * sx).ceil() as u32).min(pw.saturating_sub(x));
        let h = ((r.height() * sy).ceil() as u32).min(ph.saturating_sub(y));
        if w < 4 || h < 4 {
            continue;
        }
        let crop = full.crop_imm(x, y, w, h);
        let Some(data) = encode_image(&crop, opts.max_image_px) else { continue };
        let vector = if opts.vector_shapes && *pad > 0.0 { vector_drawing(raw, paths, &r).map(Box::new) } else { None };
        raw.glyphs.retain(|g| !r.contains_point(g.bbox.cx(), g.bbox.cy()));
        raw.rotated.retain(|(g, _)| !r.contains_point(g.bbox.cx(), g.bbox.cy()));
        raw.images.retain(|im| im.bbox.fraction_inside(&r) < 0.9);
        raw.rules.retain(|ru| ru.bbox.fraction_inside(&r.expand(1.0)) < 0.9);
        raw.boxes.retain(|b| b.bbox.fraction_inside(&r.expand(1.0)) < 0.9);
        raw.images.push(RawImage { bbox: r, data, behind_text: false, overlay: false, vector, alt: None });
    }
    Ok(())
}

/// Graphic annotations as transparent pictures: the pixels that differ between
/// the page rendered with and without its annotations.
fn render_overlays(page: &PdfPage, tf: &PageTransform, raw: &mut RawPage, rects: &[Rect], opts: &ExtractOptions) -> anyhow::Result<()> {
    let scale = opts.graphics_dpi / 72.0;
    let with = tf.render_with(page, scale, true)?.to_rgba8();
    let without = tf.render_with(page, scale, false)?.to_rgba8();
    let (pw, ph) = with.dimensions();
    let (sx, sy) = (pw as f32 / raw.width, ph as f32 / raw.height);
    for r in rects {
        let r = r.expand(2.0).intersection(&Rect::new(0.0, 0.0, raw.width, raw.height));
        let x = (r.x0 * sx).floor().max(0.0) as u32;
        let y = (r.y0 * sy).floor().max(0.0) as u32;
        let w = ((r.width() * sx).ceil() as u32).min(pw.saturating_sub(x));
        let h = ((r.height() * sy).ceil() as u32).min(ph.saturating_sub(y));
        if w < 2 || h < 2 {
            continue;
        }
        let mut img = image::RgbaImage::new(w, h);
        let mut any = false;
        for j in 0..h {
            for i in 0..w {
                let (px, py) = (x + i, y + j);
                let a = with.get_pixel(px, py);
                // Only pixels unlike every neighbour of the plain rendering: the two
                // renderings place text a fraction of a pixel apart.
                let mut d = i32::MAX;
                for ny in py.saturating_sub(1)..=(py + 1).min(ph - 1) {
                    for nx in px.saturating_sub(1)..=(px + 1).min(pw - 1) {
                        let b = without.get_pixel(nx, ny);
                        d = d.min((0..3).map(|k| (a[k] as i32 - b[k] as i32).abs()).max().unwrap_or(0));
                    }
                }
                if d > 24 {
                    img.put_pixel(i, j, image::Rgba([a[0], a[1], a[2], 255]));
                    any = true;
                }
            }
        }
        if !any {
            continue;
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        raw.images.push(RawImage {
            bbox: Rect::new(x as f32 / sx, y as f32 / sy, (x + w) as f32 / sx, (y + h) as f32 / sy),
            data: ImageData { bytes: png, ext: "png", px_width: w, px_height: h },
            behind_text: false,
            overlay: true,
            vector: None,
            alt: None,
        });
    }
    Ok(())
}

/// Renders a full page to an RGB image at the given DPI (used by OCR).
pub fn render_page(page: &PdfPage, dpi: f32) -> anyhow::Result<DynamicImage> {
    // Same orientation as the extracted text, so OCR and crops line up.
    PageTransform::for_text(page).render(page, dpi / 72.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_detection() {
        assert!(is_garbage('\u{FFFD}'));
        assert!(is_garbage('\u{E012}'));
        assert!(!is_garbage('a'));
    }
}
