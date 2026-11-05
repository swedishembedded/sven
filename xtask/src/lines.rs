// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! How many production lines a source file has, for the file-size ratchet.

use std::fs;
use std::path::Path;

use anyhow::Result;

/// Production lines in `path`: everything outside an item compiled only for
/// tests (`#[cfg(test)]`, or `#[cfg(all(test, ...))]`).
///
/// The ratchet exists to stop production files sprawling. Counting raw lines
/// made it count tests too, so it penalised exactly what it should reward: a
/// well-tested file could sit far over the limit on test code alone, and
/// adding a single test to any allowlisted file failed the build until
/// someone re-blessed it. Test modules are `//! …` inline by convention here,
/// so this scans for `#[cfg(test)]` and skips to the end of the item that
/// follows, tracking brace depth.
pub(crate) fn count_lines(path: &Path) -> Result<usize> {
    let text = fs::read_to_string(path).unwrap_or_default();
    Ok(count_production_lines(&text))
}

fn count_production_lines(text: &str) -> usize {
    let mut count = 0usize;
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        if !compiled_only_for_tests(line) {
            count += 1;
            continue;
        }
        // Skip the attribute and the item it guards. Depth only starts
        // tracking once the item's first `{` is seen, so attributes and a
        // `mod foo;` declaration on the following line are both handled.
        let mut depth = 0isize;
        let mut opened = false;
        for body in lines.by_ref() {
            depth += body.matches('{').count() as isize;
            depth -= body.matches('}').count() as isize;
            if depth > 0 {
                opened = true;
            }
            if opened && depth <= 0 {
                break;
            }
            if !opened && body.trim_end().ends_with(';') {
                break;
            }
        }
    }
    count
}

/// Whether `line` is a `cfg` attribute whose item exists only in a test
/// build: `test`, or `all(...)` with `test` among its conditions. `any(test,
/// ...)` does not count: a feature compiles that item into production too.
fn compiled_only_for_tests(line: &str) -> bool {
    let Some(predicate) = line
        .trim()
        .strip_prefix("#[cfg(")
        .and_then(|rest| rest.strip_suffix(")]"))
    else {
        return false;
    };
    if predicate.trim() == "test" {
        return true;
    }
    let Some(conditions) = predicate
        .trim()
        .strip_prefix("all(")
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in conditions.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&conditions[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&conditions[start..]);
    parts.iter().any(|part| part.trim() == "test")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_compiled_only_for_tests_is_not_counted_however_its_cfg_is_spelled() {
        let file = |attr: &str| {
            format!("fn a() {{}}\n\n{attr}\nmod tests {{\n    fn t() {{\n    }}\n}}\n")
        };
        assert_eq!(count_production_lines(&file("#[cfg(test)]")), 2);
        assert_eq!(
            count_production_lines(&file("#[cfg(all(test, target_os = \"linux\"))]")),
            2
        );
        assert_eq!(count_production_lines(&file("#[cfg(all(unix, test))]")), 2);
        // Code a feature also compiles is production code.
        assert_eq!(
            count_production_lines(&file("#[cfg(any(test, feature = \"fake\"))]")),
            7
        );
        assert_eq!(
            count_production_lines(&file("#[cfg(all(not(test), unix))]")),
            7
        );
    }
}
