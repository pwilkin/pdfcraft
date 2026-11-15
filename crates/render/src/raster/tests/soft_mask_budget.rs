//! The soft masks a page holds stay within `hayro::MAX_HELD_MASK_PIXELS` (vendored hayro patch
//! 36). Every mask is the size of the canvas and the scene keeps each one until it is
//! composited; the cache key holds the CTM, so a 71 KB page setting one soft mask under a
//! slightly different matrix for each of its fills drew and held a 4 MB mask a fill until the
//! process aborted.

use super::*;
use std::sync::Mutex;

/// The page is `SIDE` points square, rendered at 72 dpi: a mask is 4 MiB, so the budget holds
/// `masks_in_budget()`.
const SIDE: u64 = 2048;

fn masks_in_budget() -> u64 {
    hayro::MAX_HELD_MASK_PIXELS / (SIDE * SIDE)
}

/// Where square `i` sits: 10 × 10 points, ten to a row.
fn square(i: u64) -> (u64, u64) {
    ((i % 10) * 100 + 5, (i / 10) * 100 + 5)
}

/// Where a fill under the first square's matrix and mask sits, once the others are done.
const AGAIN: (u64, u64) = (1005, 1005);

/// A page that fills `fills` black squares, each under the matrix that puts it in place and the
/// soft mask `/M`, a luminosity mask that is white all over: each is a mask of its own. Then it
/// fills one more square under the first square's matrix and mask, at [`AGAIN`].
fn page(fills: u64) -> Vec<u8> {
    let mut content = String::new();
    for i in 0..fills {
        let (x, y) = square(i);
        content.push_str(&format!("q 1 0 0 1 {x} {y} cm /M gs 0 0 10 10 re f Q\n"));
    }
    let (x, y) = square(0);
    content.push_str(&format!("q 1 0 0 1 {x} {y} cm /M gs {} {} 10 10 re f Q\n", AGAIN.0 - x, AGAIN.1 - y));
    let group = format!("1 g 0 0 {SIDE} {SIDE} re f");
    let objs: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {SIDE} {SIDE}] /Contents 4 0 R /Resources << /ExtGState << /M 5 0 R >> >> >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 6 0 R >> >>".into(),
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 {SIDE} {SIDE}] /Group << /S /Transparency /CS /DeviceGray >> /Length {} >>\nstream\n{group}\nendstream",
            group.len()
        ),
    ];
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
    /// Offscreen pixels drawn, in masks' worth: here only the masks are drawn offscreen.
    masks_drawn: u64,
}

impl Rendered {
    /// The colour in the middle of the square at PDF `(x, y)`; pixel rows run top down.
    fn at(&self, (x, y): (u64, u64)) -> [u8; 3] {
        let i = (((SIDE - (y + 5)) * SIDE + x + 5) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
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
    let before = hayro::offscreen_draws().pixels;
    let pixmap = hayro::render(&parsed.pages()[0], &RenderCache::new(), &settings, &RenderSettings { bg_color: WHITE, ..Default::default() });
    let pixels = hayro::offscreen_draws().pixels - before;
    let truncated = *truncated.lock().unwrap();
    assert_eq!((u64::from(pixmap.width()), u64::from(pixmap.height())), (SIDE, SIDE));
    Rendered { rgba: pixmap.data_as_u8_slice().to_vec(), truncated, masks_drawn: pixels / (SIDE * SIDE) }
}

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE_PX: [u8; 3] = [255, 255, 255];

/// Two masks more than the budget holds: the fills whose masks fit are drawn, those past it are
/// left out and the page says so. A mask already drawn still paints. The budget is given back
/// once the page is done, so a second render draws the same.
#[test]
fn soft_masked_fills_past_the_mask_budget_are_left_out_and_said_to_be() {
    let fit = masks_in_budget();
    let fills = fit + 2;
    let pdf = page(fills);
    for pass in 0..2 {
        let page = render(pdf.clone());
        assert_eq!(page.masks_drawn, fit, "pass {pass}: masks drawn for {fills} fills");
        assert!(page.truncated, "pass {pass}: fills were left out and the page says so");
        for i in 0..fills {
            let want = if i < fit { BLACK } else { WHITE_PX };
            assert_eq!(page.at(square(i)), want, "pass {pass}: square {i}");
        }
        assert_eq!(page.at(AGAIN), BLACK, "pass {pass}: a mask already drawn paints again");
    }
}

/// Within the budget every fill is drawn, one mask each, and nothing is said to be left out.
#[test]
fn soft_masked_fills_within_the_mask_budget_are_all_drawn() {
    let fills = masks_in_budget();
    let page = render(page(fills));
    assert!(!page.truncated, "nothing left out");
    assert_eq!(page.masks_drawn, fills, "one mask a matrix");
    for i in 0..fills {
        assert_eq!(page.at(square(i)), BLACK, "square {i}");
    }
    assert_eq!(page.at(AGAIN), BLACK, "the first mask paints again");
}
