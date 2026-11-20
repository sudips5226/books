//! Mapping of PDF (PostScript) font names to Word font families and style flags.

/// Result of analysing a PDF font name.
#[derive(Clone, Debug, PartialEq)]
pub struct FontInfo {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
}

/// Well-known PostScript base names and the family Word should use.
const KNOWN: &[(&str, &str)] = &[
    ("timesnewromanps", "Times New Roman"),
    ("timesnewroman", "Times New Roman"),
    ("timesroman", "Times New Roman"),
    ("times", "Times New Roman"),
    ("nimbusromno9l", "Times New Roman"),
    ("nimbusroman", "Times New Roman"),
    ("liberationserif", "Times New Roman"),
    ("texgyretermes", "Times New Roman"),
    ("arialmt", "Arial"),
    ("arial", "Arial"),
    ("helvetica", "Arial"),
    ("helveticaneue", "Helvetica Neue"),
    ("nimbussansl", "Arial"),
    ("nimbussans", "Arial"),
    ("liberationsans", "Arial"),
    ("texgyreheros", "Arial"),
    ("couriernewpsmt", "Courier New"),
    ("couriernew", "Courier New"),
    ("courier", "Courier New"),
    ("nimbusmonl", "Courier New"),
    ("nimbusmono", "Courier New"),
    ("liberationmono", "Courier New"),
    ("symbolmt", "Symbol"),
    ("symbol", "Symbol"),
    ("zapfdingbats", "Wingdings"),
    ("wingdings", "Wingdings"),
    ("calibri", "Calibri"),
    ("cambria", "Cambria"),
    ("cambriamath", "Cambria Math"),
    ("candara", "Candara"),
    ("consolas", "Consolas"),
    ("constantia", "Constantia"),
    ("corbel", "Corbel"),
    ("georgia", "Georgia"),
    ("verdana", "Verdana"),
    ("tahoma", "Tahoma"),
    ("trebuchetms", "Trebuchet MS"),
    ("garamond", "Garamond"),
    ("bookantiqua", "Book Antiqua"),
    ("palatinolinotype", "Palatino Linotype"),
    ("palatino", "Palatino Linotype"),
    ("centurygothic", "Century Gothic"),
    ("segoeui", "Segoe UI"),
    ("lucidaconsole", "Lucida Console"),
    ("lucidasans", "Lucida Sans"),
    ("dejavusans", "DejaVu Sans"),
    ("dejavuserif", "DejaVu Serif"),
    ("dejavusansmono", "DejaVu Sans Mono"),
    ("cmr", "Cambria"),
    ("cmbx", "Cambria"),
    ("cmti", "Cambria"),
    ("cmss", "Calibri"),
    ("cmtt", "Courier New"),
    ("lmroman", "Cambria"),
    ("lmsans", "Calibri"),
    ("lmmono", "Courier New"),
    ("sfrm", "Cambria"),
];

const BOLD_TOKENS: &[&str] = &[
    "bold", "black", "heavy", "semibold", "demibold", "demi", "extrabold", "ultrabold", "bd", "bdit",
];
const ITALIC_TOKENS: &[&str] = &["italic", "oblique", "it", "bdit", "ital", "slanted", "kursiv"];
/// Style suffix tokens that are removed from the family name.
const STYLE_TOKENS: &[&str] = &[
    "bold", "italic", "oblique", "regular", "roman", "book", "medium", "light", "thin", "black",
    "heavy", "semibold", "demibold", "demi", "extrabold", "ultrabold", "condensed", "cond",
    "narrow", "mt", "ps", "psmt", "bd", "it", "bdit", "ital", "normal", "lt",
    "extralight", "ultralight", "semilight", "slanted",
];

/// Analyses a PDF font name such as `"ABCDEF+TimesNewRomanPS-BoldItalicMT"`.
pub fn analyse_font_name(raw: &str) -> FontInfo {
    // Strip the 6-letter subset prefix ("ABCDEF+").
    let name = match raw.split_once('+') {
        Some((prefix, rest)) if prefix.len() == 6 && prefix.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => raw,
    };
    let name = name.trim();
    if name.is_empty() {
        return FontInfo { family: "Times New Roman".into(), bold: false, italic: false };
    }

    // Split into base and style part on the first '-' or ','.
    let (base, style_part) = match name.find(['-', ',', '_']) {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, ""),
    };

    let words_style = split_camel(style_part);
    let mut words_base = split_camel(base);

    let lower_all = name.to_ascii_lowercase();
    let style_words_lower: Vec<String> = words_style.iter().map(|w| w.to_ascii_lowercase()).collect();

    let mut bold = style_words_lower.iter().any(|w| BOLD_TOKENS.contains(&w.as_str()))
        || lower_all.contains("bold")
        || lower_all.contains("black")
        || lower_all.contains("heavy");
    let mut italic = style_words_lower.iter().any(|w| ITALIC_TOKENS.contains(&w.as_str()))
        || lower_all.contains("italic")
        || lower_all.contains("oblique");

    // Some fonts encode the style in the base itself ("ArialBold", "Arial,BoldItalic").
    while let Some(last) = words_base.last() {
        let l = last.to_ascii_lowercase();
        if words_base.len() > 1 && STYLE_TOKENS.contains(&l.as_str()) {
            if BOLD_TOKENS.contains(&l.as_str()) {
                bold = true;
            }
            if ITALIC_TOKENS.contains(&l.as_str()) {
                italic = true;
            }
            words_base.pop();
        } else {
            break;
        }
    }

    // TeX fonts: cmbx = bold extended, cmti = text italic, etc.
    let compact: String = words_base.concat().to_ascii_lowercase();
    let compact_digits_trimmed = compact.trim_end_matches(|c: char| c.is_ascii_digit());
    match compact_digits_trimmed {
        "cmbx" | "cmb" | "cmssbx" => bold = true,
        "cmti" | "cmsl" | "cmmi" | "cmbxti" => italic = true,
        _ => {}
    }
    if compact_digits_trimmed == "cmbxti" {
        bold = true;
    }

    let family = KNOWN
        .iter()
        .find(|(k, _)| compact_digits_trimmed == *k)
        .or_else(|| KNOWN.iter().find(|(k, _)| k.len() >= 5 && compact_digits_trimmed.starts_with(k)))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| {
            let joined = words_base.join(" ");
            if joined.is_empty() { "Times New Roman".to_string() } else { joined }
        });

    FontInfo { family, bold, italic }
}

/// Splits "TimesNewRomanPS" into ["Times", "New", "Roman", "PS"].
fn split_camel(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' || c == '-' || c == ',' || c == '_' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = (c.is_uppercase() && prev.is_lowercase())
                || (c.is_uppercase() && prev.is_uppercase() && next_lower)
                || (c.is_ascii_digit() != prev.is_ascii_digit());
            if boundary {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Generic class of a family name, when it can be told from the name.
pub fn generic_of(family: &str) -> crate::model::Generic {
    use crate::model::Generic;
    let f = family.to_ascii_lowercase();
    const MONO: &[&str] = &["courier", "consolas", "mono", "menlo", "monaco", "lucida console", "code", "typewriter", "cmtt"];
    const SANS: &[&str] = &[
        "arial", "helvetica", "calibri", "verdana", "tahoma", "segoe", "trebuchet", "sans", "gothic", "corbel", "candara",
        "myriad", "frutiger", "futura", "gill", "lato", "roboto", "open sans", "source sans", "noto sans", "ubuntu", "inter",
        "franklin", "univers", "avenir", "carlito", "arimo",
    ];
    const SERIF: &[&str] = &[
        "times", "roman", "georgia", "cambria", "garamond", "palatino", "book antiqua", "serif", "minion", "baskerville",
        "caslon", "bodoni", "century", "constantia", "caladea", "tinos", "charter", "cmr", "computer modern", "latin modern",
        "lmroman", "utopia", "bookman",
    ];
    if MONO.iter().any(|k| f.contains(k)) {
        Generic::Mono
    } else if SANS.iter().any(|k| f.contains(k)) {
        Generic::Sans
    } else if SERIF.iter().any(|k| f.contains(k)) {
        Generic::Serif
    } else {
        Generic::Unknown
    }
}

/// Returns a generic fallback for unknown families based on PDF font flags.
pub fn generic_family(serif: bool, monospace: bool) -> &'static str {
    if monospace {
        "Courier New"
    } else if serif {
        "Times New Roman"
    } else {
        "Arial"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_and_style() {
        let f = analyse_font_name("ABCDEF+TimesNewRomanPS-BoldItalicMT");
        assert_eq!(f.family, "Times New Roman");
        assert!(f.bold && f.italic);

        let f = analyse_font_name("Arial,Bold");
        assert_eq!(f.family, "Arial");
        assert!(f.bold && !f.italic);

        let f = analyse_font_name("Helvetica-Oblique");
        assert_eq!(f.family, "Arial");
        assert!(f.italic && !f.bold);

        let f = analyse_font_name("QWERTY+Calibri-Light");
        assert_eq!(f.family, "Calibri");
        assert!(!f.bold);

        let f = analyse_font_name("CMBX12");
        assert!(f.bold);

        let f = analyse_font_name("MyriadPro-Semibold");
        assert_eq!(f.family, "Myriad Pro");
        assert!(f.bold);

        let f = analyse_font_name("SourceSansPro");
        assert_eq!(f.family, "Source Sans Pro");
    }

    #[test]
    fn camel() {
        assert_eq!(split_camel("TimesNewRomanPS"), vec!["Times", "New", "Roman", "PS"]);
        assert_eq!(split_camel("CMR10"), vec!["CMR", "10"]);
    }
}
