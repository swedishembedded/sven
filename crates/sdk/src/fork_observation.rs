// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements faithful replay of recorded agent sessions, for its
// clients. If your team needs expertise in agent evaluation or session replay then you can
// procure our services by sending an email to info@swedishembedded.com.

//! Comparing a replayed observation with the recorded one.
//!
//! The sandbox injects values that belong to it, not to the work: the attempt's
//! temporary directory, the clock, and the numbers a copied tree allocates, plus the
//! note the harness adds to a repeated result. A replayed observation is compared with
//! those removed and with nothing else: what the student caused stays in the text, and
//! its tree is checked by digest rather than here.

/// The observation `text` with the values the *sandbox* injects removed, so a
/// replay in a fresh workspace can be compared to a recording made in another
/// one: the per-attempt temporary directory a workspace, a `.sven` state
/// symlink or a traceback names (every attempt has its own, so the path always
/// differs), the clock's timestamp column in an `ls -l`-style listing (a replay
/// runs later, so every directory and the symlink show a new time), and the
/// numbers on the sandbox's own entries - `.` and `..` describe the temporary
/// directory the workspace was copied into, and `.sven`'s size is the length of
/// its target's path, so both move between attempts. The advisory note the
/// harness appends to a repeated result is not the tool's output either.
///
/// Nothing the student caused is removed: file names, sizes, link targets
/// outside the temporary directory, command output and exit text stay, and the
/// student's tree is checked by its digest (`RecordedState::workspace`) rather
/// than by this text.
pub(crate) fn without_harness_values(text: &str) -> std::borrow::Cow<'_, str> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if !text.contains(".tmp")
        && !MONTHS.iter().any(|m| text.contains(m))
        && !text.contains(HARNESS_NOTE)
    {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut lines: Vec<String> = Vec::with_capacity(text.lines().count());
    for line in text.split('\n') {
        if line.trim_start().starts_with(HARNESS_NOTE) {
            continue;
        }
        lines.push(without_listing_values(line, &MONTHS));
    }
    // A note that closed the observation leaves its blank line behind.
    let mut out = lines.join("\n");
    out.truncate(out.trim_end().len());
    std::borrow::Cow::Owned(out)
}

/// The first words of the note the harness appends to a result a previous call
/// returned unchanged.
const HARNESS_NOTE: &str = "[sven note:";

/// One line of `without_harness_values`: the sandbox's numbers removed from an
/// `ls -l`-style listing, whitespace collapsed, and every path into a temporary
/// directory reduced to `<tmp>`.
fn without_listing_values(line: &str, months: &[&str; 12]) -> String {
    // Whitespace runs are collapsed, so a listing's aligned columns compare
    // equal without depending on the width of a number.
    let fields: Vec<&str> = line.split_whitespace().collect();
    // A listing's block total counts what the copy allocated, not the student's
    // work, so two listings of the same tree can total differently.
    if matches!(fields.as_slice(), ["total", digits] if digits.chars().all(|c| c.is_ascii_digit()))
    {
        return "total".to_string();
    }
    let is_listing = fields.first().is_some_and(|f| {
        f.len() >= 10 && f.chars().next().is_some_and(|c| "[-dlbcps]".contains(c))
    });
    // `ls -l` puts the name after mode, links, owner, group, size and the three
    // fields of a timestamp; a listing whose month is there is one.
    let stamp = is_listing
        && fields.len() >= 9
        && months.contains(&fields[5])
        && (fields[7].contains(':') || fields[7].len() == 4);
    let sandbox = stamp && matches!(fields.get(8), Some(&".") | Some(&"..") | Some(&".sven"));
    let mut kept: Vec<&str> = Vec::with_capacity(fields.len());
    for (j, f) in fields.iter().enumerate() {
        if sandbox {
            // The sandbox entry itself, by its mode and its name: everything
            // between and after describes the temporary directory.
            if j == 0 || j == 8 {
                kept.push(f);
            }
            continue;
        }
        if stamp && (5..8).contains(&j) {
            if j == 5 {
                kept.push("<time>");
            }
            continue;
        }
        kept.push(f);
    }
    let mut out = String::with_capacity(line.len());
    for (k, f) in kept.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.push_str(&without_temp_path(f));
    }
    out
}

/// `token` with a path into a temporary directory reduced to `<tmp>`, so what
/// it names beside that path stays comparable: `"…/.tmpVAHVM8/tree/x.py",`
/// becomes `"<tmp>/tree/x.py",` and `/…/.tmpXY` becomes `<tmp>`.
fn without_temp_path(token: &str) -> std::borrow::Cow<'_, str> {
    // Where the `.tmpXXXX` path component starts, if the token has one.
    let at = match token.rfind("/.tmp") {
        Some(slash) => slash + 1,
        None if token.starts_with(".tmp") => 0,
        None => return std::borrow::Cow::Borrowed(token),
    };
    let bytes = token.as_bytes();
    let mut end = at + 4;
    while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        end += 1;
    }
    // Where the path itself starts: after the quote, parenthesis or assignment
    // that introduced it, else at the token's start.
    let start = token[..at]
        .rfind(['"', '\'', '(', '=', ':', ','])
        .map_or(0, |i| i + 1);
    let mut out = String::with_capacity(token.len() - (end - start) + 5);
    out.push_str(&token[..start]);
    out.push_str("<tmp>");
    out.push_str(&token[end..]);
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod harness_value_tests {
    use super::without_harness_values as n;

    #[test]
    fn the_sandbox_entries_of_a_listing_are_the_same_observation_in_another_attempt() {
        // The observation of `ls -la` in a workspace root, recorded in one attempt
        // and replayed in a fresh one: the student's own files are identical, and
        // what differs belongs to the sandbox - the clock, the attempt's temporary
        // directory, the `.sven` symlink that points into it and its target's
        // length, and the link count of `..`, which counts the entries the
        // sandbox makes beside the workspace.
        let recorded = "total 16\ndrwx------+       4 user user 4096 Oct 24 19:26 .\ndrwx------+       6 user user 4096 Oct 24 19:26 ..\nlrwxrwxrwx+       1 user user   41 Oct 24 19:26 .sven -> /sandbox/attempt-a/.tmpAAAAAA/state\n-rw-------+       1 user user 4365 Jan  1  1970 README.md\ndrwx------+       2 user user 4096 Jan  1  1970 tests";
        let replayed = "total 16\ndrwx------+       4 user user 4096 Oct 25 18:31 .\ndrwx------+       4 user user 4096 Oct 25 18:31 ..\nlrwxrwxrwx+       1 user user   43 Oct 25 18:31 .sven -> /sandbox/attempt-b/.tmpBBBBBB/state\n-rw-------+       1 user user 4365 Jan  1  1970 README.md\ndrwx------+       2 user user 4096 Jan  1  1970 tests";
        assert_eq!(n(recorded), n(replayed));
    }

    #[test]
    fn the_block_total_of_a_copy_of_the_same_tree_is_not_compared() {
        // A real refusal: two listings of the same directory by the same `ls -la`,
        // differing in the total, because the tree was copied rather than written.
        let recorded = "src:\ntotal 28\ndrwx------+  4 user user 4096 Oct 24 19:26 ..\n-rw-------+  1 user user 4289 Jan  1  1970 graph.lua";
        let replayed = "src:\ntotal 8\ndrwx------+  4 user user 4096 Oct 25 18:55 ..\n-rw-------+  1 user user 4289 Jan  1  1970 graph.lua";
        assert_eq!(n(recorded), n(replayed));
        // The entries themselves are still compared.
        assert_ne!(
            n(recorded),
            n("src:\ntotal 8\ndrwx------+  4 user user 4096 Oct 25 18:55 ..\n-rw-------+  1 user user 4290 Jan  1  1970 graph.lua")
        );
    }

    #[test]
    fn a_traceback_from_another_attempt_names_the_same_file() {
        // A real refusal: the student's own test run raised in the workspace,
        // and only the attempt's temporary directory differs.
        let recorded = "Traceback (most recent call last):\n  File \"<stdin>\", line 7, in <module>\n  File \"/sandbox/attempt-a/.tmpAAAAAA/tree/store/memo.py\", line 34, in __init__\n    raise ValueError(\"size must be at least 1\")\nValueError: size must be at least 1";
        let replayed = "Traceback (most recent call last):\n  File \"<stdin>\", line 7, in <module>\n  File \"/sandbox/attempt-b/.tmpBBBBBB/tree/store/memo.py\", line 34, in __init__\n    raise ValueError(\"size must be at least 1\")\nValueError: size must be at least 1";
        assert_eq!(n(recorded), n(replayed));
        // The file the traceback names is still compared.
        assert_ne!(
            n(recorded),
            n(&replayed.replace("store/memo.py\", line 34", "store/cache.py\", line 34"))
        );
    }

    #[test]
    fn the_note_the_harness_adds_to_a_repeated_result_is_not_the_observation() {
        let recorded = "ValueError: size must be at least 1\n\n[exit code 1]";
        let replayed = "ValueError: size must be at least 1\n\n[exit code 1]\n\n[sven note: this exact call returned this exact result once before in this run, and the result is unchanged. Repeating it will not give you new information; take a different action.]";
        assert_eq!(n(recorded), n(replayed));
        assert_ne!(
            n(recorded),
            n("ValueError: size must be at least 2\n\n[exit code 1]")
        );
    }

    #[test]
    fn a_changed_student_entry_beside_the_sandbox_entries_is_still_not_normalised_away() {
        let a = "total 16\ndrwx------+ 4 user user 4096 Oct 24 19:26 .\n-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        assert_ne!(
            n(a),
            n("total 16\ndrwx------+ 4 user user 4096 Oct 25 18:31 .\n-rw-------+ 1 user user 4366 Jan  1  1970 README.md")
        );
        assert_ne!(
            n(a),
            n("total 16\ndrwx------+ 4 user user 4096 Oct 25 18:31 .\n-rw-------+ 1 user user 4365 Jan  1  1970 README.txt")
        );
    }

    #[test]
    fn a_listing_from_another_attempt_and_another_day_is_the_same_observation() {
        let recorded = "lrwxrwxrwx+   1 user user   41 Oct 24 19:26 .sven -> /sandbox/attempt-a/.tmpAAAAAA/state\n-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        let replayed = "lrwxrwxrwx+ 1 user user 41 Oct 25 18:19 .sven -> /sandbox/attempt-b/.tmpBBBBBB/state\n-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        assert_eq!(n(recorded), n(replayed));
    }

    #[test]
    fn a_changed_name_size_or_sentence_is_not_normalised_away() {
        let a = "-rw-------+ 1 user user 4365 Jan  1  1970 README.md";
        assert_ne!(
            n(a),
            n("-rw-------+ 1 user user 4366 Jan  1  1970 README.md")
        );
        assert_ne!(
            n(a),
            n("-rw-------+ 1 user user 4365 Jan  1  1970 README.txt")
        );
        assert_ne!(
            n("all seven symptoms behave correctly"),
            n("all six symptoms behave correctly")
        );
    }

    #[test]
    fn text_without_a_temporary_path_or_a_month_is_borrowed_unchanged() {
        assert!(matches!(n("3 passed"), std::borrow::Cow::Borrowed(_)));
    }
}
