//! Scan clean-up before OCR: skew and orientation, and choosing the OCR
//! language from the document's text.

use image::{DynamicImage, GenericImageView, GrayImage, Luma, Rgb, RgbImage};

/// Rotates an image by `degrees` counter-clockwise (as seen on screen),
/// keeping its size; uncovered areas become white.
pub fn rotate(img: &DynamicImage, degrees: f32) -> DynamicImage {
    let src = img.to_rgb8();
    let (w, h) = src.dimensions();
    let (s, c) = degrees.to_radians().sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut out = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
    for y in 0..h {
        for x in 0..w {
            // Inverse mapping (y grows downwards: a visual CCW turn by θ maps
            // (dx, dy) to (dx·c + dy·s, −dx·s + dy·c)).
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let sx = dx * c - dy * s + cx - 0.5;
            let sy = dx * s + dy * c + cy - 0.5;
            if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f32 || sy > (h - 1) as f32 {
                continue;
            }
            // Bilinear interpolation.
            let (x0, y0) = (sx.floor() as u32, sy.floor() as u32);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let p = |xx: u32, yy: u32| src.get_pixel(xx, yy).0;
            let (a, b, cc, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
            let mut px = [0u8; 3];
            for k in 0..3 {
                let top = a[k] as f32 * (1.0 - fx) + b[k] as f32 * fx;
                let bot = cc[k] as f32 * (1.0 - fx) + d[k] as f32 * fx;
                px[k] = (top * (1.0 - fy) + bot * fy).round() as u8;
            }
            out.put_pixel(x, y, Rgb(px));
        }
    }
    DynamicImage::ImageRgb8(out)
}

/// Dark pixels of a downscaled grey version, as (x, y) in the small image.
fn ink_points(img: &DynamicImage, max_side: u32) -> (Vec<(f32, f32)>, u32, u32) {
    let (w, h) = img.dimensions();
    let k = (w.max(h) as f32 / max_side as f32).max(1.0);
    let small: GrayImage = if k > 1.0 {
        img.resize((w as f32 / k) as u32, (h as f32 / k) as u32, image::imageops::FilterType::Triangle).to_luma8()
    } else {
        img.to_luma8()
    };
    let mut pts = Vec::new();
    for (x, y, Luma([v])) in small.enumerate_pixels() {
        if *v < 128 {
            pts.push((x as f32, y as f32));
        }
    }
    // Keep the work bounded on very dark pages.
    if pts.len() > 250_000 {
        let step = pts.len() / 250_000 + 1;
        pts = pts.into_iter().step_by(step).collect();
    }
    (pts, small.width(), small.height())
}

fn profile_score(pts: &[(f32, f32)], h: u32, w: u32, degrees: f32) -> f64 {
    let (s, c) = degrees.to_radians().sin_cos();
    let n = (h + w) as usize * 2 + 4;
    let off = (w + h) as f32;
    let mut bins = vec![0u32; n];
    for &(x, y) in pts {
        let v = y * c + x * s + off;
        let i = v as usize;
        if i < n {
            bins[i] += 1;
        }
    }
    bins.iter().map(|&b| (b as f64) * (b as f64)).sum()
}

/// Estimated skew of the text lines in degrees (positive: lines rise to
/// the right, i.e. the content is turned counter-clockwise). Zero when no
/// clear skew is found.
pub fn estimate_skew(img: &DynamicImage) -> f32 {
    let (pts, w, h) = ink_points(img, 1200);
    if pts.len() < 500 {
        return 0.0;
    }
    let score = |a: f32| profile_score(&pts, h, w, a);
    let base = score(0.0);
    let mut best = (0.0f32, base);
    let mut a = -6.0f32;
    while a <= 6.0 + 1e-3 {
        let sc = score(a);
        if sc > best.1 {
            best = (a, sc);
        }
        a += 0.25;
    }
    // Refine.
    let (coarse, _) = best;
    let mut a = coarse - 0.25;
    while a <= coarse + 0.25 + 1e-3 {
        let sc = score(a);
        if sc > best.1 {
            best = (a, sc);
        }
        a += 0.05;
    }
    // Text lines give a markedly sharper profile when aligned.
    if best.0.abs() < 0.2 || best.1 < base * 1.02 {
        return 0.0;
    }
    best.0
}

/// Score of an OCR result: confident characters.
pub fn ocr_score(words: &[crate::ocr::OcrWord]) -> f32 {
    words
        .iter()
        .filter(|w| w.confidence >= 50.0)
        .map(|w| w.text.chars().filter(|c| c.is_alphanumeric()).count() as f32 * w.confidence / 100.0)
        .sum()
}

/// Rotates by quarter turns counter-clockwise.
pub fn quarter_turn(img: &DynamicImage, quarters: u8) -> DynamicImage {
    match quarters % 4 {
        1 => img.rotate270(),
        2 => img.rotate180(),
        3 => img.rotate90(),
        _ => img.clone(),
    }
}

// ---------------------------------------------------------------------------
// Language
// ---------------------------------------------------------------------------

/// Directories where Tesseract language data may be installed.
pub fn tessdata_dirs(explicit: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = explicit {
        v.push(p.to_path_buf());
    }
    if let Some(p) = crate::ocr::bundled_tessdata() {
        v.push(p);
    }
    if let Ok(p) = std::env::var("TESSDATA_PREFIX") {
        let p = std::path::PathBuf::from(p);
        v.push(p.join("tessdata"));
        v.push(p);
    }
    for p in [
        "/usr/share/tesseract-ocr/5/tessdata",
        "/usr/share/tesseract-ocr/4.00/tessdata",
        "/usr/share/tessdata",
        "/usr/local/share/tessdata",
        "/opt/homebrew/share/tessdata",
        "C:\\Program Files\\Tesseract-OCR\\tessdata",
    ] {
        v.push(p.into());
    }
    v.retain(|p| p.is_dir());
    v
}

/// Installed Tesseract languages (codes such as "eng", "deu", "chi_sim").
pub fn available_languages(explicit: Option<&std::path::Path>) -> Vec<String> {
    let mut langs: Vec<String> = Vec::new();
    for dir in tessdata_dirs(explicit) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(code) = name.strip_suffix(".traineddata") {
                    if code != "osd" && code != "equ" && !langs.iter().any(|l| l == code) {
                        langs.push(code.to_string());
                    }
                }
            }
        }
        if !langs.is_empty() {
            break;
        }
    }
    langs.sort();
    langs
}

/// Tesseract language code for a text, if it is recognisable.
pub fn detect_language(text: &str) -> Option<String> {
    let info = whatlang::detect(text)?;
    if !info.is_reliable() && text.chars().filter(|c| c.is_alphabetic()).count() < 200 {
        return None;
    }
    let code = match info.lang().code() {
        "cmn" => "chi_sim",
        "pes" => "fas",
        "nob" => "nor",
        c => c,
    };
    Some(code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_image(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
        // Text-like lines: dashes along rows.
        for row in (60..h - 60).step_by(30) {
            for x in 60..w - 60 {
                if (x / 7) % 4 != 3 {
                    for dy in 0..6 {
                        img.put_pixel(x, row + dy, Rgb([0, 0, 0]));
                    }
                }
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn skew_round_trip() {
        let img = lines_image(900, 700);
        assert_eq!(estimate_skew(&img), 0.0);
        for angle in [2.0f32, -3.5] {
            let turned = rotate(&img, angle);
            let est = estimate_skew(&turned);
            assert!((est - angle).abs() < 0.3, "rotated by {angle}, estimated {est}");
            // Undoing the estimate makes the lines horizontal again.
            let fixed = rotate(&turned, -est);
            assert!(estimate_skew(&fixed).abs() < 0.3);
        }
    }

    #[test]
    fn languages() {
        assert_eq!(detect_language("Dies ist ein deutscher Text über die Umwandlung von Dokumenten, der lang genug ist, um erkannt zu werden.").as_deref(), Some("deu"));
        assert_eq!(detect_language("This is an English sentence that is long enough for the detector to be confident about it.").as_deref(), Some("eng"));
    }
}
