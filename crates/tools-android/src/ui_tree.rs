// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Parsing, matching and change-detection over an Android view hierarchy as
//! produced by `uiautomator dump`.
//!
//! This is the deterministic half of locating a UI element. A vision
//! grounding model answers "where is this phrase, roughly" with no ground
//! truth and no confidence; the view hierarchy answers "this exact node has
//! this exact text, it is clickable, and these are its exact pixel bounds" -
//! or answers that no such node exists, which a generative grounding model
//! structurally cannot do (it always emits some box).
//!
//! Three jobs, all pure functions over the dumped XML so they are testable
//! without a device:
//!
//! - [`parse`]: XML into a flat [`Element`] list retaining parent links.
//! - [`find`]: a natural-language target phrase into the element to tap,
//!   resolved to the nearest clickable ancestor.
//! - [`signature`]: a stable digest of the hierarchy, so "did this action
//!   change anything" is answerable without pixel diffing.
//!
//! Swedish Embedded AB implements solutions for deterministic Android UI
//! element resolution for its clients. If your team needs expertise in
//! on-device test automation that does not guess, you can procure our
//! services by sending an email to info@swedishembedded.com.

/// Pixel bounds of a node: `[x0,y0][x1,y1]`, inclusive of the top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
}

impl Bounds {
    /// Centre point, the coordinate a tap on this element should use.
    #[must_use]
    pub fn center(&self) -> (i64, i64) {
        ((self.x0 + self.x1) / 2, (self.y0 + self.y1) / 2)
    }

    /// Area in square pixels, used to prefer the tightest of several
    /// equally-scoring matches.
    #[must_use]
    pub fn area(&self) -> i64 {
        (self.x1 - self.x0).max(0) * (self.y1 - self.y0).max(0)
    }
}

/// One node of the hierarchy, flattened but retaining its parent's index so
/// a matched label can be resolved to the clickable container that actually
/// handles the tap.
#[derive(Debug, Clone)]
pub struct Element {
    pub text: String,
    pub desc: String,
    pub resource_id: String,
    pub class: String,
    pub clickable: bool,
    pub enabled: bool,
    pub bounds: Bounds,
    pub parent: Option<usize>,
}

/// Why [`find`] picked an element - carried into the tool's answer so a
/// failing run's log says how the target was resolved, not just where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// The normalized target equalled the node's text/description exactly.
    Exact,
    /// One normalized string contained the other.
    Contains,
    /// Enough words overlapped to clear the threshold.
    Fuzzy,
}

/// A resolved target: which element to tap, how confident, and how it was
/// found. `label` is the on-screen string that matched.
#[derive(Debug, Clone)]
pub struct Match {
    pub index: usize,
    pub score: f64,
    pub kind: MatchKind,
    pub label: String,
}

/// Minimum score [`find`] will accept. Below this the honest answer is "not
/// on this screen" - the answer that makes a UI test able to fail.
pub const MATCH_THRESHOLD: f64 = 0.5;

/// Flatten a `uiautomator dump` XML into [`Element`]s, keeping each node's
/// parent index. Unparseable attributes degrade to defaults rather than
/// failing the whole dump: a single odd node must not cost a run its screen.
#[must_use]
pub fn parse(xml: &str) -> Vec<Element> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut out: Vec<Element> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) | Err(_) => break,
            Ok(ev @ (Event::Start(_) | Event::Empty(_))) => {
                // A self-closing `<node ... />` has no matching `End`, so it
                // must never be pushed onto the parent stack - doing so
                // reparents every later sibling under it.
                let (e, opens_scope) = match &ev {
                    Event::Start(e) => (e, true),
                    Event::Empty(e) => (e, false),
                    _ => unreachable!("matched Start or Empty above"),
                };
                if e.name().as_ref() != b"node" {
                    buf.clear();
                    continue;
                }
                let mut el = Element {
                    text: String::new(),
                    desc: String::new(),
                    resource_id: String::new(),
                    class: String::new(),
                    clickable: false,
                    enabled: true,
                    bounds: Bounds {
                        x0: 0,
                        y0: 0,
                        x1: 0,
                        y1: 0,
                    },
                    parent: stack.last().copied(),
                };
                for attr in e.attributes().flatten() {
                    let value = attr.unescape_value().unwrap_or_default().into_owned();
                    match attr.key.as_ref() {
                        b"text" => el.text = value,
                        b"content-desc" => el.desc = value,
                        b"resource-id" => el.resource_id = value,
                        b"class" => el.class = value,
                        b"clickable" => el.clickable = value == "true",
                        b"enabled" => el.enabled = value == "true",
                        b"bounds" => {
                            if let Some(b) = parse_bounds(&value) {
                                el.bounds = b;
                            }
                        }
                        _ => {}
                    }
                }
                out.push(el);
                if opens_scope {
                    stack.push(out.len() - 1);
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"node" {
                    stack.pop();
                }
            }
            _ => {}
        }
        buf.clear();
    }

    out
}

/// `"[x0,y0][x1,y1]"` -> [`Bounds`].
fn parse_bounds(raw: &str) -> Option<Bounds> {
    let nums: Vec<i64> = raw
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums[..] {
        [x0, y0, x1, y1] => Some(Bounds { x0, y0, x1, y1 }),
        _ => None,
    }
}

/// Case-, accent- and punctuation-insensitive form used for all comparisons.
/// Step text is written by a human in plain ASCII ("Kom igang"); the screen
/// renders real orthography ("Kom igång"). Both fold to the same key here.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        let folded = fold_char(ch);
        for c in folded.chars() {
            if c.is_alphanumeric() {
                out.push(c);
                last_space = false;
            } else if !last_space {
                out.push(' ');
                last_space = true;
            }
        }
    }
    out.trim_end().to_string()
}

/// Map one character to its accent-free lowercase equivalent.
fn fold_char(ch: char) -> String {
    let lower = ch.to_lowercase().next().unwrap_or(ch);
    match lower {
        'å' | 'ä' | 'à' | 'á' | 'â' | 'ã' => "a".to_string(),
        'ö' | 'ò' | 'ó' | 'ô' | 'õ' | 'ø' => "o".to_string(),
        'è' | 'é' | 'ê' | 'ë' => "e".to_string(),
        'ì' | 'í' | 'î' | 'ï' => "i".to_string(),
        'ù' | 'ú' | 'û' | 'ü' => "u".to_string(),
        'ñ' => "n".to_string(),
        'ç' => "c".to_string(),
        'ß' => "ss".to_string(),
        other => other.to_string(),
    }
}

/// Dice coefficient over whitespace tokens: `2|A∩B| / (|A|+|B|)`, counting
/// each shared token once. Used only as the last resort, below exact and
/// containment matching.
fn token_overlap(a: &str, b: &str) -> f64 {
    let at: Vec<&str> = a.split(' ').filter(|s| !s.is_empty()).collect();
    let bt: Vec<&str> = b.split(' ').filter(|s| !s.is_empty()).collect();
    if at.is_empty() || bt.is_empty() {
        return 0.0;
    }
    let shared = at.iter().filter(|t| bt.contains(t)).count();
    2.0 * shared as f64 / (at.len() + bt.len()) as f64
}

/// Score one on-screen label against the target, or `None` below threshold.
fn score_label(target: &str, label: &str) -> Option<(f64, MatchKind)> {
    if label.is_empty() {
        return None;
    }
    if target == label {
        return Some((1.0, MatchKind::Exact));
    }
    if label.contains(target) || target.contains(label) {
        let (short, long) = if target.len() < label.len() {
            (target.len(), label.len())
        } else {
            (label.len(), target.len())
        };
        // A containment match is worth more the less padding it carries, so
        // "Sign in" inside "Sign in with Mobile BankID" outranks a one-word
        // hit inside a paragraph.
        let ratio = short as f64 / long as f64;
        return Some((0.6 + 0.3 * ratio, MatchKind::Contains));
    }
    let overlap = token_overlap(target, label);
    (overlap >= MATCH_THRESHOLD).then_some((overlap * 0.9, MatchKind::Fuzzy))
}

/// Walk up from `index` to the nearest element that actually handles a tap.
/// An Android label is routinely a non-clickable `TextView` inside a
/// clickable `Button`/`ViewGroup`; tapping the label's own centre works only
/// by luck, and tapping a match that has no clickable ancestor at all is
/// what silently does nothing.
fn clickable_ancestor(elements: &[Element], index: usize) -> Option<usize> {
    let mut cur = Some(index);
    while let Some(i) = cur {
        if elements[i].clickable && elements[i].enabled {
            return Some(i);
        }
        cur = elements[i].parent;
    }
    None
}

/// Resolve a natural-language target to the element to tap, or `None` when
/// the target is not on this screen.
///
/// Returning `None` is the point: it is the answer a generative grounding
/// model cannot give, and without it a UI test cannot fail.
#[must_use]
pub fn find(elements: &[Element], target: &str) -> Option<Match> {
    let needle = normalize(target);
    if needle.is_empty() {
        return None;
    }

    let mut best: Option<Match> = None;
    for (i, el) in elements.iter().enumerate() {
        if !el.enabled {
            continue;
        }
        for raw in [&el.text, &el.desc] {
            let Some((score, kind)) = score_label(&needle, &normalize(raw)) else {
                continue;
            };
            let Some(target_index) = clickable_ancestor(elements, i) else {
                continue;
            };
            let candidate = Match {
                index: target_index,
                score,
                kind,
                label: raw.clone(),
            };
            let better = match &best {
                None => true,
                Some(b) => {
                    (candidate.score, -elements[candidate.index].bounds.area())
                        > (b.score, -elements[b.index].bounds.area())
                }
            };
            if better {
                best = Some(candidate);
            }
        }
    }
    best
}

/// Strip Private Use Area codepoints and the separator debris they leave.
///
/// Icon fonts put glyphs at U+E000..U+F8FF, so a real `content-desc` reads
/// `"\u{f1c9}, CHANGE TO BUSINESS"`. Those codepoints are invisible noise in
/// an error message (and already ignored by [`normalize`], which keeps only
/// alphanumerics), so only the human-facing label needs cleaning.
fn display_label(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if ('\u{e000}'..='\u{f8ff}').contains(&c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    cleaned
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every label a human could actually tap on this screen, for the error a
/// not-found step reports. A test that fails saying only "not found" costs
/// an engineer a device; one that lists what WAS on screen usually does not.
#[must_use]
pub fn candidates(elements: &[Element]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, el) in elements.iter().enumerate() {
        if !el.clickable || !el.enabled {
            continue;
        }
        let label = display_label(&if !el.desc.is_empty() {
            el.desc.clone()
        } else if !el.text.is_empty() {
            el.text.clone()
        } else {
            descendant_label(elements, i).unwrap_or_default()
        });
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

/// First non-empty text/description beneath `ancestor`, so a clickable
/// container that carries no label of its own still names itself.
fn descendant_label(elements: &[Element], ancestor: usize) -> Option<String> {
    elements.iter().enumerate().find_map(|(i, el)| {
        let mut cur = el.parent;
        while let Some(p) = cur {
            if p == ancestor {
                if !el.text.is_empty() {
                    return Some(el.text.clone());
                }
                if !el.desc.is_empty() {
                    return Some(el.desc.clone());
                }
                return None;
            }
            cur = elements[p].parent;
        }
        let _ = i;
        None
    })
}

/// A stable digest of everything about this screen an action could change.
///
/// Deliberately over the hierarchy rather than over pixels: a screenshot of
/// an idle device is NOT stable (a status-bar clock or a live network-rate
/// readout repaints constantly), so a pixel diff answers "something changed"
/// every single time and can never detect an action that did nothing. The
/// dumped hierarchy of an idle device is byte-identical across seconds.
#[must_use]
pub fn signature(elements: &[Element]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for el in elements {
        hasher.update(el.class.as_bytes());
        hasher.update([0]);
        hasher.update(el.resource_id.as_bytes());
        hasher.update([0]);
        hasher.update(el.text.as_bytes());
        hasher.update([0]);
        hasher.update(el.desc.as_bytes());
        hasher.update([0]);
        hasher.update([u8::from(el.clickable), u8::from(el.enabled)]);
        hasher.update(
            format!(
                "{},{},{},{}",
                el.bounds.x0, el.bounds.y0, el.bounds.x1, el.bounds.y1
            )
            .as_bytes(),
        );
        hasher.update([b'\n']);
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A faithful slice of a real `uiautomator dump` from the app this was
    /// built against: a clickable Button carrying the label only as its
    /// `content-desc`, with a NON-clickable TextView child carrying the same
    /// text. Resolving either to the Button is the whole point of [`find`].
    const LOGIN_SCREEN: &str = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<hierarchy rotation="0">
  <node index="0" text="" resource-id="" class="android.widget.FrameLayout" package="se.betalo.androidapp" content-desc="" clickable="false" enabled="true" bounds="[0,0][1220,2712]">
    <node index="0" text="" resource-id="" class="android.view.ViewGroup" package="se.betalo.androidapp" content-desc="Sweden" clickable="true" enabled="true" bounds="[800,210][1190,330]">
      <node index="0" text="Sweden" resource-id="" class="android.widget.TextView" package="se.betalo.androidapp" content-desc="" clickable="false" enabled="true" bounds="[959,245][1091,294]" />
    </node>
    <node index="1" text="Pay bills or send money" resource-id="" class="android.widget.TextView" package="se.betalo.androidapp" content-desc="" clickable="false" enabled="true" bounds="[45,1075][1174,1321]" />
    <node index="2" text="" resource-id="se.betalo.androidapp:id/sign_in" class="android.widget.Button" package="se.betalo.androidapp" content-desc="Sign in with Mobile BankID" clickable="true" enabled="true" bounds="[185,1777][1036,1939]">
      <node index="0" text="Sign in with Mobile BankID" resource-id="" class="android.widget.TextView" package="se.betalo.androidapp" content-desc="" clickable="false" enabled="true" bounds="[377,1825][994,1891]" />
    </node>
    <node index="3" text="" resource-id="" class="android.widget.Button" package="se.betalo.androidapp" content-desc="CHANGE TO BUSINESS" clickable="true" enabled="true" bounds="[310,1984][910,2097]">
      <node index="0" text="Kom ig&#229;ng" resource-id="" class="android.widget.TextView" package="se.betalo.androidapp" content-desc="" clickable="false" enabled="true" bounds="[424,2011][862,2070]" />
    </node>
  </node>
</hierarchy>"#;

    fn parsed() -> Vec<Element> {
        parse(LOGIN_SCREEN)
    }

    #[test]
    fn parse_reads_every_node_with_its_bounds_and_clickability() {
        let els = parsed();
        assert_eq!(els.len(), 8, "one Element per <node>, nesting flattened");

        let button = els
            .iter()
            .find(|e| e.resource_id.ends_with("sign_in"))
            .expect("the sign-in Button is in the dump");
        assert_eq!(button.desc, "Sign in with Mobile BankID");
        assert!(button.clickable);
        assert_eq!(
            button.bounds,
            Bounds {
                x0: 185,
                y0: 1777,
                x1: 1036,
                y1: 1939
            }
        );
        assert_eq!(button.bounds.center(), (610, 1858));
    }

    #[test]
    fn parse_decodes_xml_entities_in_text() {
        let els = parsed();
        assert!(
            els.iter().any(|e| e.text == "Kom igång"),
            "&#229; must decode to a real 'å', not stay escaped"
        );
    }

    /// The label a user reads is a non-clickable TextView; the tap has to
    /// land on its clickable ancestor or it does nothing at all. This is the
    /// exact shape that made a real run tap empty space and still pass.
    #[test]
    fn a_match_on_a_label_resolves_to_its_clickable_ancestor() {
        let els = parsed();
        let m = find(&els, "Sign in with Mobile BankID").expect("target is on screen");
        assert!(
            els[m.index].clickable,
            "resolved element must be the clickable Button, not the TextView"
        );
        assert_eq!(els[m.index].bounds.center(), (610, 1858));
    }

    #[test]
    fn an_exact_target_scores_above_a_fuzzy_one() {
        let els = parsed();
        let exact = find(&els, "Sign in with Mobile BankID").expect("exact target");
        assert_eq!(exact.kind, MatchKind::Exact);
        assert!((exact.score - 1.0).abs() < 1e-9);
    }

    /// Step text is written by a human in plain ASCII; the screen uses real
    /// Swedish orthography. These must match.
    #[test]
    fn diacritics_and_case_do_not_prevent_a_match() {
        let els = parsed();
        let m = find(&els, "kom igang").expect("'Kom igång' must match 'kom igang'");
        assert!(els[m.index].clickable);
        assert_eq!(m.kind, MatchKind::Exact);
    }

    /// The defect this whole module exists to fix: a phrase that is simply
    /// not on screen must resolve to nothing, so the step can fail. A
    /// generative grounding model always returns a box and so always
    /// "succeeds".
    #[test]
    fn a_target_that_is_not_on_screen_is_not_found() {
        let els = parsed();
        assert!(
            find(&els, "Logga in").is_none(),
            "'Logga in' is not on this screen and must not resolve to anything"
        );
    }

    #[test]
    fn candidates_lists_what_a_human_could_actually_tap() {
        let els = parsed();
        let c = candidates(&els);
        assert!(c.iter().any(|s| s == "Sign in with Mobile BankID"));
        assert!(c.iter().any(|s| s == "CHANGE TO BUSINESS"));
        assert!(
            !c.iter().any(|s| s == "Pay bills or send money"),
            "a non-clickable paragraph is not a tap candidate"
        );
    }

    /// Real `content-desc`s carry icon-font glyphs from the Private Use
    /// Area. They are invisible noise, and a not-found error is only useful
    /// if a human can read the list of what WAS on screen.
    #[test]
    fn candidate_labels_drop_icon_font_glyphs() {
        let els = parse(
            r#"<hierarchy><node class="android.widget.Button" text="" content-desc="&#xf1c9;, CHANGE TO BUSINESS" clickable="true" enabled="true" bounds="[0,0][10,10]" /></hierarchy>"#,
        );
        assert_eq!(candidates(&els), vec!["CHANGE TO BUSINESS".to_string()]);
    }

    #[test]
    fn a_signature_is_stable_for_the_same_screen_and_differs_for_another() {
        let a = signature(&parsed());
        let b = signature(&parsed());
        assert_eq!(a, b, "same hierarchy must digest identically");
        assert!(!a.is_empty());

        let changed = parse(&LOGIN_SCREEN.replace("Sign in with Mobile BankID", "Signing in..."));
        assert_ne!(
            a,
            signature(&changed),
            "a screen whose content changed must digest differently"
        );
    }
}
