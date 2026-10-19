// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The adapter conformance suite.
//!
//! Each public function is one named case that takes a fresh, empty store and
//! panics if the store breaks the [`MemoryStore`] contract. An adapter runs
//! them all through [`memory_conformance_tests!`](crate::memory_conformance_tests),
//! which expands to one `#[tokio::test]` per case:
//!
//! ```ignore
//! sven_memory_api::memory_conformance_tests!(|| async { MyStore::open_empty().await });
//! ```
//!
//! The cases cover what any implementation must do. What depends on the
//! engine - schema migration, startup without a home directory, damaged rows -
//! is tested in the adapter's own crate.
//!
//! Swedish Embedded AB implements solutions for verifiable agent memory
//! storage for its clients. If your team needs expertise in proving that a
//! database adapter keeps tenants apart then you can procure our services by
//! sending an email to info@swedishembedded.com.

mod annotations;
mod fixtures;
mod generations;
mod isolation;
mod retrieval;

pub use annotations::annotation_with_a_stale_expected_revision_gets_conflict_with_current;
pub use generations::{
    a_query_vector_finds_by_meaning_what_no_word_matches, a_query_vector_must_fit_the_generation,
    a_query_vector_stays_inside_the_callers_spaces, embeddings_must_match_the_generation_spec,
    query_against_a_retired_or_foreign_generation_is_refused,
};
pub use isolation::{
    a_unit_of_another_space_is_not_found_by_id_search_or_forget,
    scope_filter_runs_inside_the_query_and_does_not_starve_visible_hits, writes_need_a_write_grant,
};
pub use retrieval::{
    delete_of_a_source_removes_its_units_from_every_read_path, ids_on_the_wire_are_opaque_strings,
    ingest_refuses_units_that_are_not_verbatim_slices,
    like_fallback_and_tag_filter_escape_wildcards,
    retrieved_text_is_quoted_as_untrusted_and_never_becomes_instructions,
    unchanged_reingest_creates_no_new_work,
};

/// Expands to one `#[tokio::test]` per conformance case, each opening a fresh
/// store with `$open`, an expression evaluating to a closure that returns a
/// future of the store.
#[macro_export]
macro_rules! memory_conformance_tests {
    ($open:expr) => {
        $crate::memory_conformance_tests!(@cases $open;
            scope_filter_runs_inside_the_query_and_does_not_starve_visible_hits,
            a_unit_of_another_space_is_not_found_by_id_search_or_forget,
            writes_need_a_write_grant,
            like_fallback_and_tag_filter_escape_wildcards,
            ids_on_the_wire_are_opaque_strings,
            ingest_refuses_units_that_are_not_verbatim_slices,
            annotation_with_a_stale_expected_revision_gets_conflict_with_current,
            unchanged_reingest_creates_no_new_work,
            query_against_a_retired_or_foreign_generation_is_refused,
            embeddings_must_match_the_generation_spec,
            a_query_vector_finds_by_meaning_what_no_word_matches,
            a_query_vector_stays_inside_the_callers_spaces,
            a_query_vector_must_fit_the_generation,
            delete_of_a_source_removes_its_units_from_every_read_path,
            retrieved_text_is_quoted_as_untrusted_and_never_becomes_instructions
        );
    };
    (@cases $open:expr; $($case:ident),+) => {
        $(
            #[tokio::test]
            async fn $case() {
                let store = ($open)().await;
                $crate::conformance::$case(&store).await;
            }
        )+
    };
}
