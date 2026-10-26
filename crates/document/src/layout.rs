// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Page layout: regions a model grounds on a page, and the order to read them.
//!
//! Whole-page OCR reads a newspaper badly: columns interleave, a model loses
//! its place after the first column and loops. Asked to ground its output
//! instead, a model such as DeepSeek-OCR returns each block with a bounding
//! box. This module parses that, orders the blocks deterministically (no chat
//! model involved), and leaves the pipeline to read each block from a crop of
//! the page at a resolution of its own.
//!
//! Swedish Embedded AB implements document layout analysis for its clients.
//! If your team needs expertise in digitising newspapers and multi-column
//! archives then you can procure our services by sending an email to
//! info@swedishembedded.com.

/// A block of a page, as a model located it.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// The model's label: `title`, `text`, `table`, `image`, ...
    pub label: String,
    /// `[x1, y1, x2, y2]` as fractions of the page, 0 at the top left.
    pub bbox: [f32; 4],
    /// What the model read in the block while grounding.
    pub text: String,
}

impl Region {
    pub fn width(&self) -> f32 {
        self.bbox[2] - self.bbox[0]
    }

    /// Blocks that carry text to read; pictures are left out of the output.
    pub fn is_textual(&self) -> bool {
        !matches!(
            self.label.as_str(),
            "image" | "figure" | "picture" | "image_caption_box" | "separator" | "line"
        )
    }

    /// Headline that opens an article (not a sub-heading inside one).
    fn is_headline(&self) -> bool {
        self.label == "title"
    }
}

/// Turns a grounded answer into regions. Model-specific formats sit behind
/// this trait; an answer with no regions means the model did not ground.
pub trait LayoutParser: Send + Sync {
    fn parse(&self, answer: &str) -> Vec<Region>;
}

/// `<|ref|>label<|/ref|><|det|>[[x1,y1,x2,y2]]<|/det|>` followed by the
/// block's text, coordinates normalised to 0..999 (DeepSeek-OCR).
#[derive(Debug, Default, Clone, Copy)]
pub struct DeepseekGrounding;

const REF_OPEN: &str = "<|ref|>";
const REF_CLOSE: &str = "<|/ref|>";
const DET_OPEN: &str = "<|det|>";
const DET_CLOSE: &str = "<|/det|>";
const GRID: f32 = 999.0;

impl LayoutParser for DeepseekGrounding {
    fn parse(&self, answer: &str) -> Vec<Region> {
        let mut regions = Vec::new();
        for chunk in answer.split(REF_OPEN).skip(1) {
            let Some((label, rest)) = chunk.split_once(REF_CLOSE) else {
                continue;
            };
            let Some((_, rest)) = rest.split_once(DET_OPEN) else {
                continue;
            };
            let Some((boxes, text)) = rest.split_once(DET_CLOSE) else {
                continue;
            };
            let Some(bbox) = union_of_boxes(boxes) else {
                continue;
            };
            regions.push(Region {
                label: label.trim().to_lowercase().replace(' ', "_"),
                bbox,
                text: text.trim().to_string(),
            });
        }
        regions
    }
}

/// The union of every `[x1,y1,x2,y2]` in `[[...],[...]]`, as page fractions.
fn union_of_boxes(s: &str) -> Option<[f32; 4]> {
    let mut union: Option<[f32; 4]> = None;
    for part in s.split('[').skip(1) {
        let numbers: Vec<f32> = part
            .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse().ok())
            .collect();
        if numbers.len() < 4 || numbers[..4].iter().any(|v| !(0.0..=GRID).contains(v)) {
            continue;
        }
        let b = [
            numbers[0] / GRID,
            numbers[1] / GRID,
            numbers[2] / GRID,
            numbers[3] / GRID,
        ];
        if b[2] <= b[0] || b[3] <= b[1] {
            continue;
        }
        union = Some(match union {
            None => b,
            Some(u) => [
                u[0].min(b[0]),
                u[1].min(b[1]),
                u[2].max(b[2]),
                u[3].max(b[3]),
            ],
        });
    }
    union
}

// ── Reading order ───────────────────────────────────────────────────────────

/// A block at least this wide cuts the page into bands (a masthead, a banner).
const WIDE: f32 = 0.75;
/// A block at most this wide is a column block.
const NARROW: f32 = 0.6;
/// Column left edges within this distance are one column.
const COLUMN_TOLERANCE: f32 = 0.025;
/// A block starts below a wide block when it is within this of its bottom.
const BAND_SLACK: f32 = 0.005;

#[derive(Debug, Clone, Copy)]
struct Column {
    left: f32,
    right: f32,
}

/// Number of columns the page's narrow blocks line up in.
pub fn column_count(regions: &[Region]) -> usize {
    columns_of(regions).len()
}

fn columns_of(regions: &[Region]) -> Vec<Column> {
    let mut narrow: Vec<&Region> = regions
        .iter()
        .filter(|r| r.is_textual() && r.width() <= NARROW)
        .collect();
    narrow.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
    let mut clusters: Vec<Vec<&Region>> = Vec::new();
    for r in narrow {
        match clusters.last_mut() {
            Some(c) if r.bbox[0] - c[0].bbox[0] <= COLUMN_TOLERANCE => c.push(r),
            _ => clusters.push(vec![r]),
        }
    }
    let mut columns: Vec<Column> = clusters
        .iter()
        .filter(|c| c.len() >= 2)
        .map(|c| {
            let median = |f: fn(&Region) -> f32| {
                let mut v: Vec<f32> = c.iter().map(|r| f(r)).collect();
                v.sort_by(f32::total_cmp);
                v[v.len() / 2]
            };
            Column {
                left: median(|r| r.bbox[0]),
                right: median(|r| r.bbox[2]),
            }
        })
        .collect();
    columns.sort_by(|a, b| a.left.total_cmp(&b.left));
    // A single column is just a page.
    if columns.len() < 2 {
        columns.clear();
    }
    columns
}

/// The columns a block spans: the first and the last it overlaps by at least
/// a third of its own width (or of the column's).
fn span(r: &Region, columns: &[Column]) -> Option<(usize, usize)> {
    let overlapping: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            let overlap = r.bbox[2].min(c.right) - r.bbox[0].max(c.left);
            overlap > 0.0 && overlap >= 0.33 * (c.right - c.left).min(r.width())
        })
        .map(|(i, _)| i)
        .collect();
    Some((*overlapping.first()?, *overlapping.last()?))
}

/// Reading order of a page's blocks, deterministic and independent of the
/// order the model listed them in.
///
/// Wide blocks cut the page into bands, read top to bottom. Within a band the
/// blocks are read column by column, except that every block belongs to the
/// headline above it that reaches its column, and an article (headline plus
/// its blocks) is read whole before the next one starts. A headline spanning
/// two columns is read before the text under it in both. Without two or more
/// columns the order is simply top to bottom, left to right.
pub fn reading_order(regions: Vec<Region>) -> Vec<Region> {
    let columns = columns_of(&regions);
    if columns.is_empty() {
        let mut out = regions;
        out.sort_by(|a, b| {
            (a.bbox[1], a.bbox[0])
                .partial_cmp(&(b.bbox[1], b.bbox[0]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        return out;
    }
    let (mut separators, rest): (Vec<Region>, Vec<Region>) =
        regions.into_iter().partition(|r| r.width() >= WIDE);
    separators.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
    let mut bands: Vec<Vec<Region>> = vec![Vec::new(); separators.len() + 1];
    for r in rest {
        let band = separators
            .iter()
            .filter(|s| s.bbox[3] <= r.bbox[1] + BAND_SLACK)
            .count();
        bands[band].push(r);
    }
    let mut out = Vec::new();
    let mut separators = separators.into_iter();
    for band in bands {
        out.extend(order_band(band, &columns));
        out.extend(separators.next());
    }
    out
}

fn order_band(band: Vec<Region>, columns: &[Column]) -> Vec<Region> {
    struct Item {
        region: Region,
        first: usize,
        last: usize,
        /// Index of the headline this block belongs to, if any.
        owner: Option<usize>,
    }
    let mut items: Vec<Item> = band
        .into_iter()
        .map(|region| {
            let (first, last) = span(&region, columns).unwrap_or((0, 0));
            Item {
                region,
                first,
                last,
                owner: None,
            }
        })
        .collect();
    let headlines: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].region.is_headline())
        .collect();
    for i in 0..items.len() {
        if headlines.contains(&i) {
            continue;
        }
        let (y, column) = (items[i].region.bbox[1], items[i].first);
        items[i].owner = headlines
            .iter()
            .copied()
            .filter(|&h| {
                items[h].region.bbox[1] <= y && items[h].first <= column && column <= items[h].last
            })
            .max_by(|&a, &b| items[a].region.bbox[1].total_cmp(&items[b].region.bbox[1]));
    }
    // A group is a headline with its blocks, or one block that continues an
    // article from the column before.
    let group_of = |i: usize| items[i].owner.unwrap_or(i);
    let mut groups: Vec<usize> = (0..items.len()).map(group_of).collect();
    groups.sort_unstable();
    groups.dedup();
    groups.sort_by(|&a, &b| {
        (items[a].first, items[a].region.bbox[1])
            .partial_cmp(&(items[b].first, items[b].region.bbox[1]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut order: Vec<usize> = Vec::with_capacity(items.len());
    for g in groups {
        let mut members: Vec<usize> = (0..items.len())
            .filter(|&i| i != g && group_of(i) == g)
            .collect();
        members.sort_by(|&a, &b| {
            (items[a].first, items[a].region.bbox[1])
                .partial_cmp(&(items[b].first, items[b].region.bbox[1]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        order.push(g);
        order.extend(members);
    }
    let mut slots: Vec<Option<Region>> = items.into_iter().map(|i| Some(i.region)).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(label: &str, b: [u16; 4], text: &str) -> Region {
        let g = |v: u16| f32::from(v) / GRID;
        Region {
            label: label.into(),
            bbox: [g(b[0]), g(b[1]), g(b[2]), g(b[3])],
            text: text.into(),
        }
    }

    #[test]
    fn deepseek_grounding_is_parsed_with_text_and_tolerates_junk() {
        let answer = "<|ref|>title<|/ref|><|det|>[[11, 26, 984, 85]]<|/det|>\n# HEAD\n\n\
                      <|ref|>text<|/ref|><|det|>[[11, 180, 240, 241],[11, 250, 240, 300]]<|/det|>\nBody.\n\n\
                      <|ref|>text<|/ref|><|det|>[[5, 5, 2000, 9]]<|/det|>\nout of range\n\
                      <|ref|>broken";
        let regions = DeepseekGrounding.parse(answer);
        assert_eq!(regions.len(), 2);
        assert_eq!(
            (regions[0].label.as_str(), regions[0].text.as_str()),
            ("title", "# HEAD")
        );
        assert!((regions[1].bbox[1] - 180.0 / 999.0).abs() < 1e-6);
        assert!(
            (regions[1].bbox[3] - 300.0 / 999.0).abs() < 1e-6,
            "boxes are united"
        );
        assert!(DeepseekGrounding
            .parse("plain markdown, no grounding")
            .is_empty());
    }

    /// The blocks of a four-column front page (coordinates from a real
    /// DeepSeek-OCR answer for a 1960 newspaper), shuffled.
    fn front_page() -> Vec<Region> {
        let t = "text";
        vec![
            r("title", [11, 26, 984, 85], "MASTHEAD"),
            r(t, [11, 180, 240, 241], "c1-a"),
            r(t, [11, 241, 240, 316], "c1-b"),
            r("title", [11, 515, 240, 540], "H-c1"),
            r(t, [11, 577, 240, 664], "c1-after-H"),
            r(t, [251, 179, 490, 211], "c2-orphan-a"),
            r(t, [251, 211, 490, 271], "c2-orphan-b"),
            r("title", [282, 308, 708, 333], "H-span-c2c3"),
            r(t, [251, 339, 490, 370], "c2-under-span"),
            r(t, [251, 380, 490, 465], "c2-under-span-2"),
            r(t, [501, 339, 738, 360], "c3-under-span"),
            r(t, [501, 360, 738, 433], "c3-under-span-2"),
            r("title", [501, 515, 984, 546], "H-span-c3c4"),
            r(t, [501, 549, 738, 589], "c3-under-H2"),
            r(t, [749, 555, 984, 620], "c4-under-H2"),
            r(t, [749, 855, 984, 905], "c4-low"),
        ]
    }

    #[test]
    fn newspaper_columns_are_read_whole_and_headlines_come_first() {
        let mut page = front_page();
        page.reverse();
        let order: Vec<String> = reading_order(page).into_iter().map(|r| r.text).collect();
        let expect = [
            "c1-a",
            "c1-b",
            "H-c1",
            "c1-after-H",
            "c2-orphan-a",
            "c2-orphan-b",
            "H-span-c2c3",
            "c2-under-span",
            "c2-under-span-2",
            "c3-under-span",
            "c3-under-span-2",
            "H-span-c3c4",
            "c3-under-H2",
            "c4-under-H2",
            "c4-low",
        ];
        // The masthead cuts the page: it is read first? No: it is a wide
        // block above everything, so the band before it is empty.
        assert_eq!(order[0], "MASTHEAD");
        assert_eq!(&order[1..], &expect);
    }

    #[test]
    fn a_page_without_columns_reads_top_to_bottom() {
        let page = vec![
            r("text", [100, 300, 900, 400], "second"),
            r("title", [100, 100, 900, 200], "first"),
            r("text", [100, 500, 900, 600], "third"),
        ];
        let order: Vec<String> = reading_order(page).into_iter().map(|r| r.text).collect();
        assert_eq!(order, ["first", "second", "third"]);
        assert_eq!(column_count(&front_page()), 4);
    }
}
