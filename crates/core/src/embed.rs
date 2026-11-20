//! Embedding the PDF's own fonts into the DOCX.
//!
//! PDFs normally carry *subsets* of their fonts, re-encoded so that the font
//! program alone no longer says which glyph is which character. We read the
//! font dictionaries with `lopdf`, combine the `ToUnicode` map with the font's
//! encoding (simple TrueType) or CID-to-glyph map (Type0 / Identity-H), and
//! rebuild a TrueType font with a proper Unicode `cmap`, a `name` table whose
//! family matches the name used in the document, and the `OS/2`/`post` tables
//! Word requires. Fonts whose licence forbids embedding (`fsType` restricted),
//! CFF-flavoured fonts and Office's standard fonts are left alone.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use lopdf::{Document, Object};

use crate::fonts::analyse_font_name;
use crate::model::*;

/// A font program found in the PDF with its Unicode → glyph mapping.
#[derive(Clone, Debug)]
pub struct PdfFontProgram {
    pub base_font: String,
    pub data: Vec<u8>,
    pub unicode_to_gid: HashMap<char, u16>,
}

/// A rebuilt font ready to be embedded.
#[derive(Clone, Debug)]
pub struct EmbeddedFont {
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub data: Vec<u8>,
}

/// Fonts every Word installation has; embedding subsets of these would only
/// make later editing worse.
const STANDARD_FONTS: &[&str] = &[
    "arial", "arial black", "arial narrow", "times new roman", "courier new", "calibri", "calibri light", "cambria",
    "cambria math", "candara", "consolas", "constantia", "corbel", "georgia", "verdana", "tahoma", "trebuchet ms",
    "segoe ui", "symbol", "wingdings", "webdings", "garamond", "book antiqua", "century gothic", "lucida console",
    "palatino linotype", "aptos", "helvetica", "times",
];

fn obj<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    doc.dereference(o).map(|(_, o)| o).unwrap_or(o)
}

fn dict_get<'a>(doc: &'a Document, d: &'a lopdf::Dictionary, key: &[u8]) -> Option<&'a Object> {
    d.get(key).ok().map(|o| obj(doc, o))
}

fn name_of(o: Option<&Object>) -> Option<&[u8]> {
    match o {
        Some(Object::Name(n)) => Some(n),
        _ => None,
    }
}

fn as_dict(o: Option<&Object>) -> Option<&lopdf::Dictionary> {
    match o {
        Some(Object::Dictionary(d)) => Some(d),
        Some(Object::Stream(s)) => Some(&s.dict),
        _ => None,
    }
}

fn stream_data(o: Option<&Object>) -> Option<Vec<u8>> {
    match o {
        Some(Object::Stream(s)) => s.decompressed_content().ok().or_else(|| Some(s.content.clone())),
        _ => None,
    }
}

/// Reads all embedded TrueType font programs of a PDF.
pub fn read_pdf_fonts(path: &Path, password: Option<&str>) -> anyhow::Result<Vec<PdfFontProgram>> {
    let bytes = std::fs::read(path)?;
    let doc = match password {
        Some(p) => Document::load_mem_with_options(&bytes, lopdf::LoadOptions::with_password(p))?,
        None => Document::load_mem(&bytes)?,
    };
    let mut out = Vec::new();
    let mut seen_programs: HashSet<(String, usize)> = HashSet::new();
    for object in doc.objects.values() {
        let Some(font) = as_dict(Some(object)) else { continue };
        if name_of(dict_get(&doc, font, b"Type")) != Some(b"Font") {
            continue;
        }
        let base_font = name_of(dict_get(&doc, font, b"BaseFont")).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
        let to_unicode = stream_data(dict_get(&doc, font, b"ToUnicode")).map(|d| parse_cmap(&d)).unwrap_or_default();
        let program = match name_of(dict_get(&doc, font, b"Subtype")) {
            Some(b"TrueType") => simple_truetype(&doc, font, &to_unicode),
            Some(b"Type0") => type0(&doc, font, &to_unicode),
            _ => None,
        };
        if let Some((data, map)) = program {
            if map.is_empty() || !seen_programs.insert((base_font.clone(), data.len())) {
                continue;
            }
            out.push(PdfFontProgram { base_font, data, unicode_to_gid: map });
        }
    }
    Ok(out)
}

fn font_file2(doc: &Document, font: &lopdf::Dictionary) -> Option<Vec<u8>> {
    let desc = as_dict(dict_get(doc, font, b"FontDescriptor"))?;
    let data = stream_data(dict_get(doc, desc, b"FontFile2"))?;
    // TrueType outlines only (Word cannot embed CFF fonts).
    let face = ttf_parser::Face::parse(&data, 0).ok()?;
    face.tables().glyf?;
    Some(data)
}

fn simple_truetype(doc: &Document, font: &lopdf::Dictionary, to_unicode: &HashMap<u32, String>) -> Option<(Vec<u8>, HashMap<char, u16>)> {
    let data = font_file2(doc, font)?;
    let face = ttf_parser::Face::parse(&data, 0).ok()?;
    // code → unicode from ToUnicode, else from the encoding.
    let mut code_to_char: BTreeMap<u32, char> = BTreeMap::new();
    let enc = dict_get(doc, font, b"Encoding");
    let base_enc = match enc {
        Some(Object::Name(n)) => Some(n.as_slice()),
        Some(Object::Dictionary(d)) => name_of(dict_get(doc, d, b"BaseEncoding")),
        _ => None,
    };
    for code in 0u32..256 {
        if let Some(c) = standard_encoding(base_enc, code as u8) {
            code_to_char.insert(code, c);
        }
    }
    if let Some(Object::Dictionary(d)) = enc {
        if let Some(Object::Array(diffs)) = dict_get(doc, d, b"Differences") {
            let mut code = 0u32;
            for item in diffs {
                match obj(doc, item) {
                    Object::Integer(i) => code = *i as u32,
                    Object::Name(n) => {
                        if let Some(c) = glyph_name_to_char(&String::from_utf8_lossy(n)) {
                            code_to_char.insert(code, c);
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    for (code, s) in to_unicode {
        let mut it = s.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            code_to_char.insert(*code, c);
        }
    }
    let cmap = face.tables().cmap?;
    let lookup = |sub_platform: ttf_parser::PlatformId, enc_id: u16, cp: u32| -> Option<u16> {
        cmap.subtables
            .into_iter()
            .find(|s| s.platform_id == sub_platform && s.encoding_id == enc_id)
            .and_then(|s| s.glyph_index(cp))
            .map(|g| g.0)
            .filter(|g| *g != 0)
    };
    let mut map = HashMap::new();
    for (code, c) in code_to_char {
        let gid = lookup(ttf_parser::PlatformId::Windows, 0, 0xF000 + code)
            .or_else(|| lookup(ttf_parser::PlatformId::Windows, 0, code))
            .or_else(|| lookup(ttf_parser::PlatformId::Macintosh, 0, code))
            .or_else(|| lookup(ttf_parser::PlatformId::Windows, 1, c as u32));
        if let Some(g) = gid {
            map.insert(c, g);
        }
    }
    Some((data, map))
}

fn type0(doc: &Document, font: &lopdf::Dictionary, to_unicode: &HashMap<u32, String>) -> Option<(Vec<u8>, HashMap<char, u16>)> {
    let enc = name_of(dict_get(doc, font, b"Encoding"))?;
    if enc != b"Identity-H" && enc != b"Identity-V" {
        return None;
    }
    let desc_font = match dict_get(doc, font, b"DescendantFonts") {
        Some(Object::Array(a)) => as_dict(a.first().map(|o| obj(doc, o)))?,
        _ => return None,
    };
    if name_of(dict_get(doc, desc_font, b"Subtype")) != Some(b"CIDFontType2") {
        return None;
    }
    let data = font_file2(doc, desc_font)?;
    let cid_to_gid: Option<Vec<u8>> = match dict_get(doc, desc_font, b"CIDToGIDMap") {
        Some(o @ Object::Stream(_)) => stream_data(Some(o)),
        _ => None,
    };
    let mut map = HashMap::new();
    for (cid, s) in to_unicode {
        let mut it = s.chars();
        let (Some(c), None) = (it.next(), it.next()) else { continue };
        let gid = match &cid_to_gid {
            Some(m) => {
                let i = *cid as usize * 2;
                m.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).unwrap_or(0)
            }
            None => *cid as u16,
        };
        if gid != 0 {
            map.insert(c, gid);
        }
    }
    Some((data, map))
}

/// Parses the `bfchar` / `bfrange` sections of a ToUnicode CMap.
pub fn parse_cmap(data: &[u8]) -> HashMap<u32, String> {
    let text = String::from_utf8_lossy(data);
    let mut map = HashMap::new();
    let tokens: Vec<&str> = tokenize(&text);
    let hex = |t: &str| -> Option<Vec<u8>> {
        let t = t.strip_prefix('<')?.strip_suffix('>')?;
        let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
        (0..t.len()).step_by(2).map(|i| u8::from_str_radix(t.get(i..i + 2).unwrap_or("0"), 16).ok()).collect()
    };
    let num = |b: &[u8]| b.iter().fold(0u32, |a, x| (a << 8) | *x as u32);
    let utf16 = |b: &[u8]| -> String {
        let units: Vec<u16> = b.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
        String::from_utf16_lossy(&units)
    };
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "beginbfchar" => {
                i += 1;
                while i + 1 < tokens.len() && tokens[i] != "endbfchar" {
                    if let (Some(src), Some(dst)) = (hex(tokens[i]), hex(tokens[i + 1])) {
                        map.insert(num(&src), utf16(&dst));
                    }
                    i += 2;
                }
            }
            "beginbfrange" => {
                i += 1;
                while i + 2 < tokens.len() && tokens[i] != "endbfrange" {
                    let (Some(lo), Some(hi)) = (hex(tokens[i]), hex(tokens[i + 1])) else {
                        i += 1;
                        continue;
                    };
                    let (lo, hi) = (num(&lo), num(&hi));
                    if tokens[i + 2] == "[" {
                        let mut j = i + 3;
                        let mut code = lo;
                        while j < tokens.len() && tokens[j] != "]" {
                            if let Some(d) = hex(tokens[j]) {
                                map.insert(code, utf16(&d));
                            }
                            code += 1;
                            j += 1;
                        }
                        i = j + 1;
                    } else {
                        if let Some(d) = hex(tokens[i + 2]) {
                            let units: Vec<u16> = d.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
                            for (k, code) in (lo..=hi.min(lo + 0xFFFF)).enumerate() {
                                let mut u = units.clone();
                                if let Some(last) = u.last_mut() {
                                    *last = last.wrapping_add(k as u16);
                                }
                                map.insert(code, String::from_utf16_lossy(&u));
                            }
                        }
                        i += 3;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    map
}

fn tokenize(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'%' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'<' if b.get(i + 1) != Some(&b'<') => {
                let start = i;
                while i < b.len() && b[i] != b'>' {
                    i += 1;
                }
                i = (i + 1).min(b.len());
                out.push(&s[start..i]);
            }
            b'[' | b']' => {
                out.push(&s[i..i + 1]);
                i += 1;
            }
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'<' | b'[' | b']') {
                    i += 1;
                }
                if i == start {
                    i += 1;
                } else {
                    out.push(&s[start..i]);
                }
            }
        }
    }
    out
}

/// WinAnsi / Standard encodings (the parts that differ from Latin-1).
fn standard_encoding(base: Option<&[u8]>, code: u8) -> Option<char> {
    const WIN_80: [u16; 32] = [
        0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0, 0,
        0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
    ];
    match code {
        0x20..=0x7E => {
            if base == Some(b"StandardEncoding") && code == 0x27 {
                Some('\u{2019}')
            } else {
                Some(code as char)
            }
        }
        0x80..=0x9F => char::from_u32(WIN_80[(code - 0x80) as usize] as u32).filter(|c| *c != '\0'),
        0xA0..=0xFF => Some(code as char),
        _ => None,
    }
}

fn glyph_name_to_char(name: &str) -> Option<char> {
    if let Some(hex) = name.strip_prefix("uni") {
        if hex.len() == 4 {
            return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
        }
    }
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                return char::from_u32(v);
            }
        }
    }
    if name.chars().count() == 1 {
        return name.chars().next();
    }
    const NAMES: &[(&str, char)] = &[
        ("space", ' '), ("exclam", '!'), ("quotedbl", '"'), ("numbersign", '#'), ("dollar", '$'), ("percent", '%'),
        ("ampersand", '&'), ("quotesingle", '\''), ("quoteright", '\u{2019}'), ("quoteleft", '\u{2018}'), ("parenleft", '('),
        ("parenright", ')'), ("asterisk", '*'), ("plus", '+'), ("comma", ','), ("hyphen", '-'), ("period", '.'), ("slash", '/'),
        ("zero", '0'), ("one", '1'), ("two", '2'), ("three", '3'), ("four", '4'), ("five", '5'), ("six", '6'), ("seven", '7'),
        ("eight", '8'), ("nine", '9'), ("colon", ':'), ("semicolon", ';'), ("less", '<'), ("equal", '='), ("greater", '>'),
        ("question", '?'), ("at", '@'), ("bracketleft", '['), ("backslash", '\\'), ("bracketright", ']'), ("underscore", '_'),
        ("braceleft", '{'), ("bar", '|'), ("braceright", '}'), ("asciitilde", '~'), ("bullet", '•'), ("endash", '–'),
        ("emdash", '—'), ("quotedblleft", '“'), ("quotedblright", '”'), ("ellipsis", '…'), ("fi", '\u{FB01}'), ("fl", '\u{FB02}'),
        ("copyright", '©'), ("registered", '®'), ("trademark", '™'), ("degree", '°'), ("eacute", 'é'), ("egrave", 'è'),
        ("agrave", 'à'), ("ccedilla", 'ç'), ("udieresis", 'ü'), ("odieresis", 'ö'), ("adieresis", 'ä'), ("germandbls", 'ß'),
        ("euro", '€'), ("sterling", '£'), ("section", '§'), ("paragraph", '¶'), ("periodcentered", '·'), ("nbspace", '\u{A0}'),
    ];
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

// ---------------------------------------------------------------------------
// TrueType rebuilding
// ---------------------------------------------------------------------------

fn read_tables(data: &[u8]) -> Option<BTreeMap<[u8; 4], Vec<u8>>> {
    let n = u16::from_be_bytes(data.get(4..6)?.try_into().ok()?) as usize;
    let mut tables = BTreeMap::new();
    for i in 0..n {
        let r = data.get(12 + i * 16..28 + i * 16)?;
        let tag: [u8; 4] = r[0..4].try_into().ok()?;
        let off = u32::from_be_bytes(r[8..12].try_into().ok()?) as usize;
        let len = u32::from_be_bytes(r[12..16].try_into().ok()?) as usize;
        tables.insert(tag, data.get(off..off + len)?.to_vec());
    }
    Some(tables)
}

fn checksum(d: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in d.chunks(4) {
        let mut b = [0u8; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(b));
    }
    sum
}

fn write_font(tables: &BTreeMap<[u8; 4], Vec<u8>>) -> Vec<u8> {
    let n = tables.len() as u16;
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow * 2 <= n {
        pow *= 2;
        log += 1;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&(pow * 16).to_be_bytes());
    out.extend_from_slice(&log.to_be_bytes());
    out.extend_from_slice(&(n * 16 - pow * 16).to_be_bytes());
    let mut offset = 12 + 16 * n as usize;
    let mut body = Vec::new();
    let mut head_offset = None;
    for (tag, data) in tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        if tag == b"head" {
            head_offset = Some(offset);
        }
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * n as usize + body.len();
    }
    out.extend_from_slice(&body);
    if let Some(h) = head_offset {
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[h + 8..h + 12].copy_from_slice(&adj.to_be_bytes());
    }
    out
}

fn cmap_table(map: &BTreeMap<u16, u16>) -> Vec<u8> {
    // Format 4: one segment per code point (subsets are small) + the 0xFFFF terminator.
    let codes: Vec<(u16, u16)> = map.iter().map(|(c, g)| (*c, *g)).filter(|(c, _)| *c != 0xFFFF).collect();
    let seg = codes.len() + 1;
    let mut sub = Vec::new();
    let mut pow = 1usize;
    let mut log = 0u16;
    while pow * 2 <= seg {
        pow *= 2;
        log += 1;
    }
    let len = 16 + 8 * seg;
    for v in [4u16, len as u16, 0, (seg * 2) as u16, (pow * 2) as u16, log, (seg * 2 - pow * 2) as u16] {
        sub.extend_from_slice(&v.to_be_bytes());
    }
    for (c, _) in &codes {
        sub.extend_from_slice(&c.to_be_bytes());
    }
    sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
    sub.extend_from_slice(&0u16.to_be_bytes());
    for (c, _) in &codes {
        sub.extend_from_slice(&c.to_be_bytes());
    }
    sub.extend_from_slice(&0xFFFFu16.to_be_bytes());
    for (c, g) in &codes {
        sub.extend_from_slice(&g.wrapping_sub(*c).to_be_bytes());
    }
    sub.extend_from_slice(&1u16.to_be_bytes());
    for _ in 0..seg {
        sub.extend_from_slice(&0u16.to_be_bytes());
    }
    let mut t = Vec::new();
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&2u16.to_be_bytes());
    // (0,3) Unicode BMP and (3,1) Windows Unicode, both pointing at the subtable.
    for (p, e) in [(0u16, 3u16), (3, 1)] {
        t.extend_from_slice(&p.to_be_bytes());
        t.extend_from_slice(&e.to_be_bytes());
        t.extend_from_slice(&20u32.to_be_bytes());
    }
    t.extend_from_slice(&sub);
    t
}

fn name_table(family: &str, sub: &str) -> Vec<u8> {
    let ps: String = format!("{}-{}", family, sub).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let full = if sub == "Regular" { family.to_string() } else { format!("{family} {sub}") };
    let records: Vec<(u16, String)> =
        vec![(1, family.to_string()), (2, sub.to_string()), (3, format!("pdf2word:{ps}")), (4, full), (6, ps)];
    let mut strings = Vec::new();
    let mut recs = Vec::new();
    for (id, s) in &records {
        let enc: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        recs.push((3u16, 1u16, 0x409u16, *id, enc.len() as u16, strings.len() as u16));
        strings.extend_from_slice(&enc);
    }
    let mut t = Vec::new();
    let count = recs.len() as u16;
    for v in [0u16, count, 6 + 12 * count] {
        t.extend_from_slice(&v.to_be_bytes());
    }
    for r in recs {
        for v in [r.0, r.1, r.2, r.3, r.4, r.5] {
            t.extend_from_slice(&v.to_be_bytes());
        }
    }
    t.extend_from_slice(&strings);
    t
}

fn os2_table(face: &ttf_parser::Face, bold: bool, italic: bool, first: u16, last: u16) -> Vec<u8> {
    let mut t = vec![0u8; 96];
    let put16 = |t: &mut Vec<u8>, o: usize, v: u16| t[o..o + 2].copy_from_slice(&v.to_be_bytes());
    let upem = face.units_per_em();
    put16(&mut t, 0, 3); // version
    put16(&mut t, 2, upem / 2); // xAvgCharWidth (approximation)
    put16(&mut t, 4, if bold { 700 } else { 400 });
    put16(&mut t, 6, 5); // width class: medium
    put16(&mut t, 8, 0); // fsType: installable
    let fs_sel: u16 = if bold || italic { (bold as u16) << 5 | italic as u16 } else { 0x40 };
    put16(&mut t, 62, fs_sel);
    put16(&mut t, 64, first);
    put16(&mut t, 66, last);
    put16(&mut t, 68, face.ascender() as u16);
    put16(&mut t, 70, face.descender() as u16);
    put16(&mut t, 72, face.line_gap() as u16);
    put16(&mut t, 74, face.ascender().max(0) as u16);
    put16(&mut t, 76, (-face.descender()).max(0) as u16);
    t[42..46].copy_from_slice(&1u32.to_be_bytes()); // ulUnicodeRange1: Basic Latin
    t[78..82].copy_from_slice(&1u32.to_be_bytes()); // ulCodePageRange1: Latin 1
    t[58..62].copy_from_slice(b"PDF2");
    t
}

fn post_table() -> Vec<u8> {
    let mut t = vec![0u8; 32];
    t[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());
    t
}

/// Rebuilds `data` as a TrueType font usable by Word for `family`.
pub fn rebuild_font(data: &[u8], map: &HashMap<char, u16>, family: &str, bold: bool, italic: bool) -> Option<Vec<u8>> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    if matches!(face.permissions(), Some(ttf_parser::Permissions::Restricted)) {
        return None;
    }
    let mut tables = read_tables(data)?;
    for required in [b"glyf", b"loca", b"head", b"hhea", b"hmtx", b"maxp"] {
        if !tables.contains_key(required) {
            return None;
        }
    }
    let bmp: BTreeMap<u16, u16> = map.iter().filter(|(c, _)| (**c as u32) < 0xFFFF).map(|(c, g)| (*c as u32 as u16, *g)).collect();
    if bmp.is_empty() {
        return None;
    }
    let first = *bmp.keys().next().unwrap();
    let last = *bmp.keys().last().unwrap();
    tables.insert(*b"cmap", cmap_table(&bmp));
    let sub = match (bold, italic) {
        (false, false) => "Regular",
        (true, false) => "Bold",
        (false, true) => "Italic",
        (true, true) => "Bold Italic",
    };
    tables.insert(*b"name", name_table(family, sub));
    let style_bits = (bold as u16) | ((italic as u16) << 1);
    match tables.get_mut(b"OS/2") {
        Some(os2) if os2.len() >= 64 => {
            // Keep licence bits; make weight and style match the slot Word will look up.
            os2[4..6].copy_from_slice(&(if bold { 700u16 } else { 400 }).to_be_bytes());
            let fs_sel = if style_bits == 0 { 0x40u16 } else { (bold as u16) << 5 | italic as u16 };
            os2[62..64].copy_from_slice(&fs_sel.to_be_bytes());
        }
        _ => {
            tables.insert(*b"OS/2", os2_table(&face, bold, italic, first, last));
        }
    }
    if let Some(head) = tables.get_mut(b"head") {
        if head.len() >= 54 {
            head[44..46].copy_from_slice(&style_bits.to_be_bytes());
            head[8..12].copy_from_slice(&0u32.to_be_bytes());
        }
    }
    tables.entry(*b"post").or_insert_with(post_table);
    // Drop tables that refer to the old encoding or are irrelevant.
    tables.remove(b"DSIG");
    let out = write_font(&tables);
    // Sanity check: the result must parse and map the characters.
    let check = ttf_parser::Face::parse(&out, 0).ok()?;
    map.keys().all(|c| check.glyph_index(*c).is_some() || (*c as u32) >= 0xFFFF).then_some(out)
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// A face: (family, bold, italic).
type FaceKey = (String, bool, bool);
/// Characters used by a face and its runs as (text, PDF width, size).
type FaceUse = (HashSet<char>, Vec<(String, f32, f32)>);

/// Characters used per (family, bold, italic) in the laid-out pages.
fn used_chars(pages: &[PageLayout]) -> HashMap<FaceKey, FaceUse> {
    let mut used: HashMap<FaceKey, FaceUse> = HashMap::new();
    fn inl(v: &[Inline], used: &mut HashMap<FaceKey, FaceUse>) {
        for i in v {
            if let Inline::Text { text, style, pdf_width, .. } = i {
                let e = used.entry((style.font.clone(), style.bold, style.italic)).or_default();
                e.0.extend(text.chars().filter(|c| !c.is_whitespace()));
                if *pdf_width > 0.0 {
                    e.1.push((text.clone(), *pdf_width, style.size));
                }
            }
        }
    }
    fn blocks(v: &[Block], used: &mut HashMap<FaceKey, FaceUse>) {
        for b in v {
            match b {
                Block::Paragraph(p) => inl(&p.inlines, used),
                Block::Table(t) => t.rows.iter().flat_map(|r| &r.cells).for_each(|c| blocks(&c.blocks, used)),
                _ => {}
            }
        }
    }
    for p in pages {
        blocks(&p.blocks, &mut used);
        for tb in &p.text_boxes {
            inl(&tb.inlines, &mut used);
        }
    }
    used
}

/// Chooses which PDF fonts to embed and rebuilds them.
pub fn select(pages: &[PageLayout], programs: &[PdfFontProgram]) -> Vec<EmbeddedFont> {
    let used = used_chars(pages);
    let mut out: Vec<EmbeddedFont> = Vec::new();
    for ((family, bold, italic), (chars, runs)) in &used {
        if STANDARD_FONTS.contains(&family.to_ascii_lowercase().as_str()) || chars.is_empty() {
            continue;
        }
        // Candidate programs: same family by name; pick one covering every character.
        let mut best: Option<&PdfFontProgram> = None;
        for p in programs {
            let info = analyse_font_name(&p.base_font);
            if !info.family.eq_ignore_ascii_case(family) {
                continue;
            }
            let covers = chars.iter().all(|c| p.unicode_to_gid.contains_key(c));
            let style_match = info.bold == *bold && info.italic == *italic;
            if covers && (best.is_none() || style_match) {
                best = Some(p);
            }
        }
        let Some(p) = best else { continue };
        let Some(data) = rebuild_font(&p.data, &p.unicode_to_gid, family, *bold, *italic) else { continue };
        // Validate the mapping: run widths with the rebuilt font must match the PDF.
        let Some(font) = crate::metrics::FontData::new(data.clone(), 0) else { continue };
        let mut ok = 0;
        let mut total = 0;
        for (text, pdf_w, size) in runs.iter().take(200) {
            let w: f32 = text.chars().filter(|c| !c.is_whitespace()).filter_map(|c| font.advance(c)).sum::<f32>() * size;
            total += 1;
            // Tracking can widen the PDF run; never narrower than the font itself.
            if *pdf_w >= w * 0.97 && *pdf_w <= w * 1.25 {
                ok += 1;
            }
        }
        if total > 0 && ok * 10 < total * 8 {
            log::debug!("not embedding {family}: glyph widths do not match the PDF");
            continue;
        }
        out.push(EmbeddedFont { family: family.clone(), bold: *bold, italic: *italic, data });
    }
    out.sort_by(|a, b| (&a.family, a.bold, a.italic).cmp(&(&b.family, b.bold, b.italic)));
    out
}

/// Obfuscates a font for storage in a DOCX package (ECMA-376 §17.8.1).
pub fn obfuscate(data: &[u8], key: &str) -> Vec<u8> {
    let hex: String = key.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let bytes: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
    let mut out = data.to_vec();
    for i in 0..32.min(out.len()) {
        out[i] ^= bytes[15 - (i % 16)];
    }
    out
}

/// A deterministic GUID-formatted key for a font.
pub fn font_key(data: &[u8], salt: usize) -> String {
    // FNV-1a 128-bit-ish from two 64-bit hashes.
    let h = |seed: u64| {
        let mut x = 0xcbf2_9ce4_8422_2325u64 ^ seed;
        for b in data.iter().step_by((data.len() / 4096).max(1)) {
            x ^= *b as u64;
            x = x.wrapping_mul(0x100_0000_01b3);
        }
        x ^ data.len() as u64
    };
    let a = h(salt as u64 * 31 + 1);
    let b = h(salt as u64 * 31 + 7);
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:04X}-{:012X}}}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        a as u16,
        (b >> 48) as u16,
        b & 0xFFFF_FFFF_FFFF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmap_parsing() {
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
            1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
            2 beginbfchar <0003> <0020> <0024> <0041> endbfchar\n\
            1 beginbfrange <0044> <0046> <0061> endbfrange\n\
            1 beginbfrange <0050> <0051> [<0066006C> <00E9>] endbfrange\nendcmap";
        let m = parse_cmap(cmap);
        assert_eq!(m.get(&3).map(String::as_str), Some(" "));
        assert_eq!(m.get(&0x24).map(String::as_str), Some("A"));
        assert_eq!(m.get(&0x46).map(String::as_str), Some("c"));
        assert_eq!(m.get(&0x50).map(String::as_str), Some("fl"));
        assert_eq!(m.get(&0x51).map(String::as_str), Some("é"));
    }

    #[test]
    fn obfuscation_roundtrip() {
        let data: Vec<u8> = (0..64u8).collect();
        let key = font_key(&data, 0);
        assert_eq!(key.len(), 38);
        let o = obfuscate(&data, &key);
        assert_ne!(o[..32], data[..32]);
        assert_eq!(o[32..], data[32..]);
        assert_eq!(obfuscate(&o, &key), data);
    }

    #[test]
    fn rebuild_real_font() {
        let path = "/usr/share/fonts/truetype/crosextra/Carlito-Regular.ttf";
        let Ok(data) = std::fs::read(path) else { return };
        let face = ttf_parser::Face::parse(&data, 0).unwrap();
        let map: HashMap<char, u16> = "Hello".chars().map(|c| (c, face.glyph_index(c).unwrap().0)).collect();
        let out = rebuild_font(&data, &map, "Test Family", true, false).unwrap();
        let f = ttf_parser::Face::parse(&out, 0).unwrap();
        assert!(f.is_bold());
        assert_eq!(crate::metrics::face_families(&f), vec!["Test Family".to_string()]);
        assert_eq!(f.glyph_index('H'), face.glyph_index('H'));
        assert_eq!(checksum(&out), 0xB1B0_AFBA);
    }
}
