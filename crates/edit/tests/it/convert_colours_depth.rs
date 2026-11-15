//! Convert colours follows forms drawing forms only so deep: a long chain of nested forms in a
//! hostile file once recursed once per form and overflowed the stack, which aborts the process.

use pdfcraft_cos::{Document, ObjRef, Object, Stream};
use pdfcraft_edit::convert_colours::MAX_DEPTH;
use pdfcraft_edit::{ColourTarget, ConvertColours, convert_colours};
use std::sync::Arc;

/// Forms in the chain: far more than a small stack holds one frame each.
const FORMS: usize = 2000;

/// One page drawing form 4, and each form `k` (object k) drawing form `k + 1`, all blue.
fn chain(forms: usize) -> Document {
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /Resources << /XObject << /X 4 0 R >> >> /Contents {} 0 R >>", forms + 4),
    ];
    for k in 4..forms + 4 {
        let next = if k + 1 < forms + 4 { format!("/Resources << /XObject << /X {} 0 R >> >> ", k + 1) } else { String::new() };
        objs.push(format!("<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] {next}/Length 8 >>\nstream\n0 0 1 rg\nendstream"));
    }
    objs.push("<< /Length 5 >>\nstream\n/X Do\nendstream".to_string());
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes());
    Document::open(Arc::new(out)).unwrap()
}

fn no_images(_: &Document, _: &Stream, _: ColourTarget) -> Option<Stream> {
    None
}

fn form_text(doc: &Document, num: u32) -> String {
    match &*doc.get(ObjRef { num, generation: 0 }) {
        Object::Stream(s) => String::from_utf8_lossy(&s.decoded().unwrap()).split_whitespace().collect::<Vec<_>>().join(" "),
        _ => panic!("not a stream"),
    }
}

#[test]
fn a_deep_chain_of_nested_forms_stops_at_the_limit_and_is_counted() {
    // A small stack, so recursing once per form overflows it long before the chain ends.
    let worker = std::thread::Builder::new().stack_size(256 * 1024).spawn(|| {
        let mut doc = chain(FORMS);
        let done = convert_colours(&mut doc, &[0], &ConvertColours::default(), &mut no_images).unwrap();
        (doc, done)
    });
    let (doc, done) = worker.unwrap().join().unwrap();
    assert_eq!(done.colours, MAX_DEPTH, "the forms within the limit are converted");
    assert_eq!(done.forms_left, 1, "the first form past the limit is recorded (what it draws is never reached)");
    let last_converted = u32::try_from(4 + MAX_DEPTH - 1).unwrap();
    assert_eq!(form_text(&doc, 4), "0.114 g");
    assert_eq!(form_text(&doc, last_converted), "0.114 g");
    assert_eq!(form_text(&doc, last_converted + 1), "0 0 1 rg", "the form past the limit is left as it was");
}
