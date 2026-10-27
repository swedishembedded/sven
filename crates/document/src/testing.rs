// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Fixture PDFs for tests of this crate and of the tool built on it. Enabled
//! by the `test-support` feature; not part of the public API.

pub enum FixturePage {
    /// Born-digital page with this text, one line per entry.
    Text(Vec<&'static str>),
    /// Image-only page filled with this gray level (below 250, so not blank).
    Scan(u8),
    /// A page with nothing on it.
    Blank,
    /// Image-only page with this many dark vertical bars separated by white
    /// gutters: a page the column detector sees columns on.
    Bars(u8),
}

impl FixturePage {
    /// The lines of a text page, joined by newlines.
    pub fn text_of(page: &Self) -> String {
        match page {
            Self::Text(lines) => lines.join("\n"),
            _ => String::new(),
        }
    }
}

/// A valid PDF with one page per entry. Text uses the standard Helvetica font.
pub fn build_pdf(pages: &[FixturePage]) -> Vec<u8> {
    build_pdf_with_outline(pages, &[])
}

/// As [`build_pdf`], plus a document outline: `(level, title, page index)`
/// entries in reading order, level 1 at the top, each nested under the nearest
/// earlier entry of a lower level.
pub fn build_pdf_with_outline(pages: &[FixturePage], outline: &[(usize, &str, usize)]) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    // 1 catalog, 2 pages, 3 font; page n uses objects 4+3n (page), 5+3n
    // (content), 6+3n (image, scans only).
    let kids: String = (0..pages.len())
        .map(|n| format!("{} 0 R ", 4 + 3 * n))
        .collect();
    // The outline's objects follow the pages: the root, then one per entry.
    let outline_root = 4 + 3 * pages.len();
    objects.push(if outline.is_empty() {
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()
    } else {
        format!("<< /Type /Catalog /Pages 2 0 R /Outlines {outline_root} 0 R >>").into_bytes()
    });
    objects.push(format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", pages.len()).into_bytes());
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for (n, page) in pages.iter().enumerate() {
        let (content, resources) = match page {
            FixturePage::Text(lines) => {
                let mut c = String::from("BT /F1 12 Tf 72 720 Td 16 TL\n");
                for line in lines {
                    c.push_str(&format!("({line}) Tj T*\n"));
                }
                c.push_str("ET");
                (c, "/Font << /F1 3 0 R >>".to_string())
            }
            FixturePage::Scan(_) | FixturePage::Bars(_) => (
                "q 612 0 0 792 0 0 cm /Im0 Do Q".to_string(),
                format!("/XObject << /Im0 {} 0 R >>", 6 + 3 * n),
            ),
            FixturePage::Blank => (String::new(), String::new()),
        };
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R /Resources << {resources} >> >>",
                5 + 3 * n
            )
            .into_bytes(),
        );
        objects.push(stream(
            &format!("<< /Length {} >>", content.len()),
            content.as_bytes(),
        ));
        objects.push(match page {
            FixturePage::Scan(gray) => stream(
                "<< /Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 4096 >>",
                &vec![*gray; 4096],
            ),
            FixturePage::Bars(n) => stream(
                "<< /Type /XObject /Subtype /Image /Width 120 /Height 64 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 7680 >>",
                &bars(*n),
            ),
            _ => b"null".to_vec(),
        });
    }
    objects.extend(outline_objects(outline, outline_root));
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for offset in offsets {
        out.extend(format!("{offset:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .bytes(),
    );
    out
}

/// The outline root (object `root`) and its items (`root + 1 + i`), linked as
/// a tree with Parent/First/Last/Prev/Next; each item points at its page.
fn outline_objects(outline: &[(usize, &str, usize)], root: usize) -> Vec<Vec<u8>> {
    if outline.is_empty() {
        return Vec::new();
    }
    let id = |i: usize| root + 1 + i;
    let parent: Vec<Option<usize>> = (0..outline.len())
        .map(|i| (0..i).rev().find(|&j| outline[j].0 < outline[i].0))
        .collect();
    let children = |p: Option<usize>| -> Vec<usize> {
        (0..outline.len()).filter(|&i| parent[i] == p).collect()
    };
    let link = |kids: &[usize]| -> String {
        match (kids.first(), kids.last()) {
            (Some(f), Some(l)) => format!(
                " /First {} 0 R /Last {} 0 R /Count {}",
                id(*f),
                id(*l),
                kids.len()
            ),
            _ => String::new(),
        }
    };
    let mut objects = vec![format!("<< /Type /Outlines{} >>", link(&children(None))).into_bytes()];
    for (i, (_, title, page)) in outline.iter().enumerate() {
        let siblings = children(parent[i]);
        let at = siblings.iter().position(|&s| s == i).unwrap();
        let mut item = format!(
            "<< /Title ({title}) /Parent {} 0 R /Dest [{} 0 R /Fit]",
            parent[i].map_or(root, id),
            4 + 3 * page
        );
        if at > 0 {
            item.push_str(&format!(" /Prev {} 0 R", id(siblings[at - 1])));
        }
        if let Some(next) = siblings.get(at + 1) {
            item.push_str(&format!(" /Next {} 0 R", id(*next)));
        }
        item.push_str(&link(&children(Some(i))));
        item.push_str(" >>");
        objects.push(item.into_bytes());
    }
    objects
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("{dict}\nstream\n").into_bytes();
    v.extend(data);
    v.extend(b"\nendstream");
    v
}

pub const PROSE: [&str; 3] = [
    "The committee reviewed the annual budget and approved the proposal for the new library.",
    "Funding will come from the regional grant, which was increased by three percent this year.",
    "Construction is expected to begin in the spring and finish before the end of next autumn.",
];

/// 120 x 64 gray image: `n` dark bars with a white gutter between them and
/// white margins.
fn bars(n: u8) -> Vec<u8> {
    let n = usize::from(n.max(1));
    let (width, margin, gutter) = (120, 10, 6);
    let bar = (width - 2 * margin - gutter * (n - 1)) / n;
    let mut row = vec![255u8; width];
    for i in 0..n {
        let from = margin + i * (bar + gutter);
        row[from..from + bar].fill(40);
    }
    (0..64).flat_map(|_| row.clone()).collect()
}
