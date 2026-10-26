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
}

/// A valid PDF with one page per entry. Text uses the standard Helvetica font.
pub fn build_pdf(pages: &[FixturePage]) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = Vec::new();
    // 1 catalog, 2 pages, 3 font; page n uses objects 4+3n (page), 5+3n
    // (content), 6+3n (image, scans only).
    let kids: String = (0..pages.len())
        .map(|n| format!("{} 0 R ", 4 + 3 * n))
        .collect();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
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
            FixturePage::Scan(_) => (
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
            _ => b"null".to_vec(),
        });
    }
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
