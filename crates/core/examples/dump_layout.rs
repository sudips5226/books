//! Prints the analysed layout of a PDF (debugging aid).
//!
//! `cargo run -p pdf2word-core --example dump_layout -- file.pdf [page]`

use pdf2word_core::model::*;
use pdf2word_core::*;
use pdfium_render::prelude::*;

fn dump(blocks: &[Block], depth: usize) {
    let ind = "  ".repeat(depth);
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                let t = p.plain_text();
                let t: String = t.chars().take(70).collect();
                println!(
                    "{ind}P [{:.0},{:.0} {:.0}x{:.0}] lines={} pitch={:.1} sb={:.1} size={} {:?} ind=({:.0},{:.0},{:.0}) {:?}",
                    p.bbox.x0, p.bbox.y0, p.bbox.width(), p.bbox.height(), p.n_lines, p.line_pitch.unwrap_or(0.0),
                    p.space_before, p.font_size, p.align, p.indent_left, p.indent_first, p.indent_right, t
                );
            }
            Block::Picture(p) => println!("{ind}IMG [{:.0},{:.0} {:.0}x{:.0}] sb={:.1}", p.bbox.x0, p.bbox.y0, p.width, p.height, p.space_before),
            Block::Rule(r) => println!("{ind}RULE [{:.0},{:.0} {:.0}] sb={:.1}", r.bbox.x0, r.bbox.y0, r.bbox.width(), r.space_before),
            Block::Table(t) => {
                println!("{ind}TABLE layout={} cols={:?} rows={} sb={:.1}", t.layout_only, t.col_widths.iter().map(|w| w.round()).collect::<Vec<_>>(), t.rows.len(), t.space_before);
                for (ri, r) in t.rows.iter().enumerate() {
                    for (ci, c) in r.cells.iter().enumerate() {
                        println!("{ind}  cell {ri},{ci} span={}", c.col_span);
                        dump(&c.blocks, depth + 2);
                    }
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let pdfium = load_pdfium(None)?;
    let doc = pdfium.load_pdf_from_file(&args[1], None)?;
    let only: Option<usize> = args.get(2).and_then(|s| s.parse().ok());
    for i in 0..doc.pages().len() as usize {
        if only.is_some_and(|o| o != i + 1) {
            continue;
        }
        let page = doc.pages().get(i as PdfPageIndex)?;
        let (raw, stats) = extract::extract_page(&doc, &page, i, &ExtractOptions::default())?;
        println!("=== page {} ({}x{}) glyphs={} images={} rules={} boxes={} stats={:?}", i + 1, raw.width, raw.height, raw.glyphs.len(), raw.images.len(), raw.rules.len(), raw.boxes.len(), stats);
        if std::env::var("DUMP_GLYPHS").is_ok() {
            for g in &raw.glyphs {
                println!("  {:?} [{:.1},{:.1},{:.1},{:.1}] base={:.1} size={} sp={} font={} math={}", g.ch, g.bbox.x0, g.bbox.y0, g.bbox.x1, g.bbox.y1, g.baseline, raw.styles[g.style].size, g.space_before, raw.styles[g.style].font, raw.styles[g.style].math);
            }
        }
        if std::env::var("DUMP_GLYPHS").is_ok() {
            for r in &raw.rules {
                println!("  rule [{:.1},{:.1},{:.1},{:.1}] {:?} w={:.2}", r.bbox.x0, r.bbox.y0, r.bbox.x1, r.bbox.y1, r.orientation, r.width);
            }
            for (g, a) in &raw.rotated {
                println!("  rot {:?} [{:.1},{:.1},{:.1},{:.1}] angle={a:.1} sp={}", g.ch, g.bbox.x0, g.bbox.y0, g.bbox.x1, g.bbox.y1, g.space_before);
            }
        }
        let l = layout::analyse_page(&raw, &LayoutOptions::default());
        println!("margins l={:.0} r={:.0} t={:.0} b={:.0} floating={}", l.margin_left, l.margin_right, l.margin_top, l.margin_bottom, l.floating.len());
        dump(&l.blocks, 0);
    }
    Ok(())
}
