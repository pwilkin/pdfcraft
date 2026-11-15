//! The tiling cells a page holds stay within `hayro::MAX_HELD_CELL_PIXELS` (vendored hayro patch
//! 35). The scene keeps every cell it is painted with until it is composited, and a fill whose
//! pattern isn't among the cells kept drew a new one: a 9.7 KB page filling small squares with
//! nine patterns of 3000 × 3000 cells, in turn, grew by 36 MB a fill until the process aborted.

use super::*;
use std::sync::Mutex;

/// Cells are `CELL` pixels a side at 72 dpi: 16 MB each, so the budget holds `cells_in_budget()`.
const CELL: u64 = 2048;

fn cells_in_budget() -> u64 {
    hayro::MAX_HELD_CELL_PIXELS / (CELL * CELL)
}

/// The grey of pattern `i`, `i` / 32.
fn grey(i: u64) -> u8 {
    (i as f64 / 32.0 * 255.0).round() as u8
}

/// Where square `i` sits on the 200 × 200 page: 10 × 10 points, six to a row.
fn square(i: u64) -> (u64, u64) {
    ((i % 6) * 30 + 5, (i / 6) * 30 + 5)
}

/// A page with `patterns` coloured tiling patterns `/P0`, `/P1` … of one `CELL`-point cell each,
/// grey `i` / 32 all over. The page sets each pattern in turn and fills two squares with it,
/// `2i` and `2i + 1`; the second fill takes the cell the first drew.
fn page(patterns: u64) -> Vec<u8> {
    let mut content = String::new();
    for i in 0..patterns {
        let ((x0, y0), (x1, y1)) = (square(2 * i), square(2 * i + 1));
        content.push_str(&format!("/Pattern cs /P{i} scn {x0} {y0} 10 10 re f {x1} {y1} 10 10 re f\n"));
    }
    let names: String = (0..patterns).map(|i| format!("/P{i} {} 0 R ", 5 + i)).collect();
    let mut objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Pattern << {names}>> >> >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    for i in 0..patterns {
        let cell = format!("{} g 0 0 {CELL} {CELL} re f", i as f64 / 32.0);
        objs.push(format!(
            "<< /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 {CELL} {CELL}] /XStep {CELL} /YStep {CELL} /Resources << >> /Length {} >>\nstream\n{cell}\nendstream",
            cell.len()
        ));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

struct Rendered {
    rgba: Vec<u8>,
    truncated: bool,
    cells_drawn: u64,
}

impl Rendered {
    /// The colour in the middle of square `i`; pixel rows run top down.
    fn square(&self, i: u64) -> [u8; 3] {
        let (x, y) = square(i);
        let at = (((200 - (y + 5)) * 200 + x + 5) * 4) as usize;
        [self.rgba[at], self.rgba[at + 1], self.rgba[at + 2]]
    }
}

fn render(pdf: Vec<u8>) -> Rendered {
    let parsed = Pdf::new(Arc::new(pdf)).unwrap();
    let truncated = Arc::new(Mutex::new(false));
    let seen = truncated.clone();
    let settings = InterpreterSettings {
        warning_sink: Arc::new(move |w| {
            if matches!(w, InterpreterWarning::ContentTruncated) {
                *seen.lock().unwrap() = true;
            }
        }),
        ..Default::default()
    };
    let before = hayro::offscreen_draws().tiling_cells;
    let pixmap = hayro::render(&parsed.pages()[0], &RenderCache::new(), &settings, &RenderSettings { bg_color: WHITE, ..Default::default() });
    let cells_drawn = hayro::offscreen_draws().tiling_cells - before;
    let truncated = *truncated.lock().unwrap();
    assert_eq!((pixmap.width(), pixmap.height()), (200, 200));
    Rendered { rgba: pixmap.data_as_u8_slice().to_vec(), truncated, cells_drawn }
}

/// Two patterns more than the budget holds: the cells that fit are drawn and their squares show
/// their grey, the fills past it are left out (white) and the page says so. The budget is given
/// back once the page is done, so a second render draws the same.
#[test]
fn pattern_fills_past_the_cell_budget_are_left_out_and_said_to_be() {
    let fit = cells_in_budget();
    let patterns = fit + 2;
    let pdf = page(patterns);
    for pass in 0..2 {
        let page = render(pdf.clone());
        assert_eq!(page.cells_drawn, fit, "pass {pass}: cells drawn for {patterns} patterns");
        assert!(page.truncated, "pass {pass}: fills were left out and the page says so");
        for i in 0..2 * patterns {
            let want = if i / 2 < fit { grey(i / 2) } else { 255 };
            let got = page.square(i);
            assert!(got.iter().all(|c| c.abs_diff(want) <= 1), "pass {pass}: square {i} is {got:?}, not grey {want}");
        }
    }
}

/// Within the budget every fill is drawn, one cell a pattern, and nothing is said to be left out.
#[test]
fn pattern_fills_within_the_cell_budget_are_all_drawn() {
    let patterns = cells_in_budget();
    let page = render(page(patterns));
    assert!(!page.truncated, "nothing left out");
    assert_eq!(page.cells_drawn, patterns, "one cell a pattern");
    for i in 0..2 * patterns {
        let (want, got) = (grey(i / 2), page.square(i));
        assert!(got.iter().all(|c| c.abs_diff(want) <= 1), "square {i} is {got:?}, not grey {want}");
    }
}
