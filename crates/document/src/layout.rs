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

    /// Whether the block is worth a request of its own: body text of some
    /// size. Headlines are set large and read well from the layout pass; a
    /// one-line block (a date, a running head) is too small to repay a request.
    pub fn needs_crop(&self) -> bool {
        const ONE_LINE: f32 = 0.02;
        self.is_textual()
            && !matches!(self.label.as_str(), "title" | "sub_title")
            && self.bbox[3] - self.bbox[1] > ONE_LINE
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
            // A block that continues into the next column lists both boxes:
            // each is a region of its own, and the text (which the model gave
            // once) stays with the first.
            let label = label.trim().to_lowercase().replace(' ', "_");
            for (i, bbox) in boxes_of(boxes).into_iter().enumerate() {
                regions.push(Region {
                    label: label.clone(),
                    bbox,
                    text: if i == 0 {
                        text.trim().to_string()
                    } else {
                        String::new()
                    },
                });
            }
        }
        regions
    }
}

/// Every `[x1,y1,x2,y2]` in `[[...],[...]]`, as page fractions; boxes out of
/// the grid or empty are skipped.
fn boxes_of(s: &str) -> Vec<[f32; 4]> {
    let mut boxes = Vec::new();
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
        if b[2] > b[0] && b[3] > b[1] {
            boxes.push(b);
        }
    }
    boxes
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
    // Columns stand side by side. A cluster that starts inside the one before
    // it is an indent (a list, a table's first cell), not a column.
    let mut apart: Vec<Column> = Vec::new();
    for c in columns {
        if apart
            .last()
            .is_none_or(|prev| c.left >= prev.right - COLUMN_TOLERANCE)
        {
            apart.push(c);
        }
    }
    let mut columns = apart;
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

// ── Fewer, larger crops ─────────────────────────────────────────────────────

/// Body blocks stacked in one column within this distance (page height) of each
/// other are one run of text.
const RUN_GAP: f32 = 0.035;
/// Left and right edges of blocks in one run agree within this.
const RUN_EDGE: f32 = 0.04;
/// A merged crop is at most this share of the page height and area: bigger
/// ones would ask the model for more than it reliably writes in one answer.
const RUN_MAX_HEIGHT: f32 = 0.6;
const RUN_MAX_AREA: f32 = 0.3;
/// A neighbour overlapped more than this by a merged crop stops the merge.
const RUN_MAX_INTRUSION: f32 = 0.1;

fn is_body(r: &Region) -> bool {
    matches!(r.label.as_str(), "text" | "list")
}

fn union(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn area(b: [f32; 4]) -> f32 {
    (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0)
}

fn intersection_area(a: [f32; 4], b: [f32; 4]) -> f32 {
    area([
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ])
}

/// Joins runs of body blocks that follow each other in reading order, stand in
/// one column and nearly touch, so that a column of twenty paragraphs is read
/// in one request instead of twenty. Headlines, sub-headings, tables and
/// pictures are never merged, and neither is a run whose crop would take in a
/// block that is not part of it.
pub fn merge_runs(ordered: Vec<Region>) -> Vec<Region> {
    let all: Vec<[f32; 4]> = ordered.iter().map(|r| r.bbox).collect();
    let mut out: Vec<Region> = Vec::new();
    // Indices (into `ordered`) of the blocks in the run being built.
    let mut members: Vec<usize> = Vec::new();
    for (i, region) in ordered.into_iter().enumerate() {
        let joined = out.last().is_some_and(|last| {
            is_body(last)
                && is_body(&region)
                && can_join(last, &region)
                && !intrudes(union(last.bbox, region.bbox), &all, &members, i)
        });
        if joined {
            let last = out.last_mut().expect("checked above");
            last.bbox = union(last.bbox, region.bbox);
            last.text = format!("{}\n\n{}", last.text, region.text)
                .trim()
                .to_string();
            members.push(i);
        } else {
            members = vec![i];
            out.push(region);
        }
    }
    out
}

fn can_join(a: &Region, b: &Region) -> bool {
    let (x, y) = (a.bbox, b.bbox);
    let gap = y[1] - x[3];
    let merged = union(x, y);
    (x[0] - y[0]).abs() <= RUN_EDGE
        && (x[2] - y[2]).abs() <= RUN_EDGE
        && (-RUN_GAP..=RUN_GAP).contains(&gap)
        && merged[3] - merged[1] <= RUN_MAX_HEIGHT
        && area(merged) <= RUN_MAX_AREA
}

/// Whether `crop` would take in a block that is not one of the run's own
/// (`members`, which excludes the candidate `next`).
fn intrudes(crop: [f32; 4], all: &[[f32; 4]], members: &[usize], next: usize) -> bool {
    all.iter().enumerate().any(|(j, b)| {
        !members.contains(&j)
            && j != next
            && area(*b) > 0.0
            && intersection_area(crop, *b) / area(*b) > RUN_MAX_INTRUSION
    })
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
        assert_eq!(regions.len(), 3, "a block with two boxes is two regions");
        assert_eq!(
            (regions[0].label.as_str(), regions[0].text.as_str()),
            ("title", "# HEAD")
        );
        assert!((regions[1].bbox[1] - 180.0 / 999.0).abs() < 1e-6);
        assert!((regions[2].bbox[1] - 250.0 / 999.0).abs() < 1e-6);
        assert_eq!(
            (regions[1].text.as_str(), regions[2].text.as_str()),
            ("Body.", "")
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
    fn indented_lines_and_tables_are_not_columns() {
        // A register page: a heading, lines indented to three depths, tables.
        let t = "text";
        let page = vec![
            r(
                "sub_title",
                [107, 105, 535, 123],
                "3.9.3 Flash key register",
            ),
            r(t, [203, 130, 882, 160], "intro"),
            r(t, [205, 166, 363, 180], "a"),
            r(t, [205, 181, 403, 195], "b"),
            r(t, [205, 202, 471, 216], "c"),
            r("table", [108, 233, 888, 326], "t"),
            r(t, [184, 344, 429, 358], "d"),
            r(t, [251, 359, 865, 387], "e"),
            r(t, [291, 388, 470, 402], "f"),
            r(t, [291, 403, 480, 417], "g"),
        ];
        assert_eq!(column_count(&page), 0);
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

    #[test]
    fn runs_of_body_blocks_in_a_column_become_one_crop() {
        let ordered = reading_order(front_page());
        let before = ordered.len();
        let merged = merge_runs(ordered);
        let texts: Vec<&str> = merged.iter().map(|r| r.text.as_str()).collect();
        // Two paragraphs in column one before the headline, and the pairs under
        // the two-column headline, are one crop each; headlines stay alone.
        assert!(merged.len() < before, "{texts:?}");
        let c1 = merged.iter().find(|r| r.text.starts_with("c1-a")).unwrap();
        assert!(
            c1.text.contains("c1-b") && !c1.text.contains("H-c1"),
            "{:?}",
            c1.text
        );
        let under = merged
            .iter()
            .find(|r| r.text.starts_with("c2-under-span"))
            .unwrap();
        assert!(under.text.contains("c2-under-span-2"));
        assert!(
            !under.text.contains("c3-under"),
            "a run never crosses columns"
        );
        assert!(merged
            .iter()
            .any(|r| r.label == "title" && r.text == "H-c1"));
    }

    #[test]
    fn a_crop_never_swallows_a_block_that_is_not_part_of_it() {
        // Two body blocks in one column with a picture box laid over the gap.
        let page = vec![
            r("text", [100, 100, 400, 300], "above"),
            r("image", [100, 290, 400, 420], "picture"),
            r("text", [100, 310, 400, 500], "below"),
        ];
        let merged = merge_runs(reading_order(page));
        assert_eq!(merged.iter().filter(|r| r.is_textual()).count(), 2);
    }

    /// A real DeepSeek-OCR-2 grounding of the 1960 newspaper's front page: 65
    /// boxes that must become a handful of column crops, not 65 requests.
    #[test]
    fn a_real_front_page_needs_far_fewer_crops_than_blocks() {
        let blocks: [(&str, [u16; 4]); 65] = [
            ("text", [9, 86, 106, 97]),
            ("text", [250, 86, 310, 97]),
            ("text", [486, 86, 586, 97]),
            ("text", [908, 89, 988, 100]),
            ("title", [67, 116, 436, 175]),
            ("text", [5, 183, 244, 242]),
            ("text", [5, 242, 244, 317]),
            ("text", [6, 317, 244, 359]),
            ("text", [6, 358, 244, 425]),
            ("text", [6, 424, 245, 501]),
            ("text", [252, 183, 491, 217]),
            ("text", [252, 217, 492, 277]),
            ("sub_title", [308, 309, 685, 330]),
            ("text", [274, 338, 486, 381]),
            ("text", [274, 385, 486, 468]),
            ("text", [274, 471, 487, 493]),
            ("text", [503, 107, 958, 123]),
            ("title", [503, 126, 929, 213]),
            ("text", [501, 215, 991, 232]),
            ("text", [708, 234, 792, 245]),
            ("text", [501, 245, 991, 277]),
            ("text", [757, 278, 991, 314]),
            ("text", [757, 314, 991, 367]),
            ("text", [503, 339, 713, 360]),
            ("text", [503, 363, 714, 459]),
            ("text", [503, 461, 714, 493]),
            ("text", [757, 367, 991, 411]),
            ("text", [757, 411, 991, 446]),
            ("text", [757, 447, 991, 500]),
            ("title", [7, 517, 336, 543]),
            ("title", [7, 551, 438, 576]),
            ("text", [5, 580, 245, 665]),
            ("text", [5, 666, 244, 705]),
            ("text", [5, 708, 244, 737]),
            ("text", [5, 740, 244, 797]),
            ("text", [5, 799, 244, 838]),
            ("sub_title", [53, 856, 192, 870]),
            ("text", [13, 873, 236, 909]),
            ("text", [13, 909, 236, 952]),
            ("text", [13, 952, 236, 980]),
            ("text", [252, 580, 492, 656]),
            ("text", [252, 657, 492, 715]),
            ("text", [252, 717, 492, 757]),
            ("text", [252, 759, 492, 832]),
            ("text", [252, 835, 492, 880]),
            ("text", [252, 882, 492, 959]),
            ("text", [252, 961, 492, 990]),
            ("title", [504, 515, 989, 545]),
            ("text", [502, 552, 742, 592]),
            ("text", [502, 592, 742, 662]),
            ("text", [502, 663, 742, 683]),
            ("sub_title", [509, 699, 657, 711]),
            ("text", [509, 715, 721, 761]),
            ("text", [749, 554, 990, 589]),
            ("text", [749, 589, 990, 624]),
            ("text", [749, 624, 990, 701]),
            ("text", [766, 701, 878, 710]),
            ("text", [749, 711, 990, 772]),
            ("title", [502, 789, 908, 850]),
            ("text", [501, 857, 741, 891]),
            ("text", [501, 890, 741, 932]),
            ("text", [501, 932, 741, 991]),
            ("text", [749, 858, 990, 910]),
            ("text", [749, 912, 990, 965]),
            ("text", [749, 966, 990, 994]),
        ];
        let page: Vec<Region> = blocks.iter().map(|(l, b)| r(l, *b, "t")).collect();
        let ordered = reading_order(page);
        let merged = merge_runs(ordered);
        let crops = merged.iter().filter(|r| r.needs_crop()).count();
        assert!(crops <= 16, "{crops} crops");
    }
}
