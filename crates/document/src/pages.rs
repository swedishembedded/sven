// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Page selections such as `1-3,7,10-`.

use crate::error::DocumentError;

/// Parse a 1-based selection ("all" or empty selects every page) into sorted,
/// unique 0-based indices below `page_count`.
pub fn parse_selection(spec: &str, page_count: usize) -> Result<Vec<usize>, DocumentError> {
    let bad = |why: &str| DocumentError::PageSelection(spec.to_string(), why.to_string());
    let trimmed = spec.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok((0..page_count).collect());
    }
    let mut picked = vec![false; page_count];
    for part in trimmed.split(',') {
        let part = part.trim();
        let (from, to) = match part.split_once('-') {
            None => {
                let n = number(part).ok_or_else(|| bad("expected a page number"))?;
                (n, n)
            }
            Some((a, b)) => {
                let from = if a.trim().is_empty() {
                    1
                } else {
                    number(a).ok_or_else(|| bad("expected a page number"))?
                };
                let to = if b.trim().is_empty() {
                    page_count
                } else {
                    number(b).ok_or_else(|| bad("expected a page number"))?
                };
                (from, to)
            }
        };
        if from > to {
            return Err(bad("a range runs backwards"));
        }
        if to > page_count {
            return Err(bad(&format!("the document has {page_count} pages")));
        }
        for p in from..=to {
            picked[p - 1] = true;
        }
    }
    Ok(picked
        .iter()
        .enumerate()
        .filter_map(|(i, on)| on.then_some(i))
        .collect())
}

fn number(s: &str) -> Option<usize> {
    s.trim().parse::<usize>().ok().filter(|n| *n >= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_lists_and_open_ends() {
        assert_eq!(parse_selection("", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_selection("2-3,1", 5).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_selection("4-", 5).unwrap(), vec![3, 4]);
        assert_eq!(parse_selection("-2", 5).unwrap(), vec![0, 1]);
    }

    #[test]
    fn out_of_range_and_garbage_are_refused() {
        assert!(parse_selection("6", 5).is_err());
        assert!(parse_selection("0", 5).is_err());
        assert!(parse_selection("3-2", 5).is_err());
        assert!(parse_selection("x", 5).is_err());
    }
}
