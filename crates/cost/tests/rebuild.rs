// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The cache-aware rebuild decision's spec: a rebuild that saves tokens still
//! costs the cached prefix behind its first changed token, and it pays only if
//! enough requests remain to earn that back.

use sven_cost::{
    assess, expected_remaining_requests, gate, record, CacheAware, CacheState, Generation,
    Necessity, RebuildCandidate,
};
use sven_model_catalog::{CachePricing, Pricing};
use sven_vocab::cost::{GateAction, GateReason, RebuildKind, Recommendation};
use sven_vocab::Amount;

fn cached() -> Pricing {
    Pricing {
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
        cache: Some(CachePricing {
            read_per_mtok: 0.3,
            write_per_mtok: 3.75,
            ttl_secs: 300,
        }),
    }
}

/// An old result near the start of a 100k-token prompt is shrunk, saving 10k:
/// everything after it must be written to the cache again.
fn early_demotion() -> RebuildCandidate {
    RebuildCandidate {
        kind: RebuildKind::Demotion,
        prompt_tokens: 100_000,
        tokens_saved: 10_000,
        first_changed_token: 20_000,
        generation: Generation::default(),
    }
}

fn warm(n: u32) -> CacheState {
    CacheState { warm_tokens: n }
}

#[test]
fn a_rebuild_that_throws_away_a_warm_cache_must_earn_it_back() {
    let c = early_demotion();
    // First request: keep reads 100k (0.03); rebuild reads 20k and writes 70k
    // (0.006 + 0.2625). After that each request reads 90k instead of 100k and
    // saves 0.003, so the 0.2385 extra pays back after 79.5 more requests.
    let few = assess(Some(&cached()), warm(100_000), &c, 10);
    assert_eq!(few.recommendation, Recommendation::Keep);
    let be = few.break_even_requests.expect("a break-even exists");
    assert!((be - 80.5).abs() < 1e-6, "{be}");

    let many = assess(Some(&cached()), warm(100_000), &c, 200);
    assert_eq!(many.recommendation, Recommendation::Rebuild);
    assert!(many.saving.value().unwrap() > 0.0);
}

#[test]
fn with_a_cold_cache_nothing_is_lost_by_rebuilding() {
    let a = assess(Some(&cached()), warm(0), &early_demotion(), 1);
    assert_eq!(a.recommendation, Recommendation::Rebuild);
    assert_eq!(a.break_even_requests, Some(1.0));
}

#[test]
fn a_provider_without_a_cache_pays_back_on_the_first_request() {
    let plain = Pricing {
        input_per_mtok: 3.0,
        output_per_mtok: 15.0,
        cache: None,
    };
    let a = assess(Some(&plain), warm(100_000), &early_demotion(), 1);
    assert_eq!(a.recommendation, Recommendation::Rebuild);
    assert!((a.saving.value().unwrap() - 10_000.0 * 3.0 / 1e6).abs() < 1e-12);
}

#[test]
fn a_rebuild_that_calls_a_model_pays_for_that_call_too() {
    let mut c = early_demotion();
    let without = assess(Some(&cached()), warm(0), &c, 5);
    c.generation = Generation {
        input_tokens: 100_000,
        output_tokens: 2_000,
    };
    let with = assess(Some(&cached()), warm(0), &c, 5);
    let extra = with.rebuild.value().unwrap() - without.rebuild.value().unwrap();
    assert!((extra - (100_000.0 * 3.0 + 2_000.0 * 15.0) / 1e6).abs() < 1e-9);
}

#[test]
fn an_undeclared_price_is_unknown_and_not_a_recommendation() {
    let a = assess(None, warm(100_000), &early_demotion(), 100);
    assert_eq!(a.recommendation, Recommendation::Unknown);
    assert_eq!(a.saving, Amount::Unknown);
    assert_eq!(a.break_even_requests, None);
}

#[test]
fn more_requests_or_more_savings_never_turn_a_rebuild_into_a_keep() {
    for warm_tokens in [0, 50_000, 100_000] {
        let mut flipped = false;
        let mut last = f64::MIN;
        for n in 1..=300 {
            let a = assess(Some(&cached()), warm(warm_tokens), &early_demotion(), n);
            let saving = a.saving.value().unwrap();
            assert!(saving >= last, "saving fell at {n} requests");
            last = saving;
            if a.recommendation == Recommendation::Rebuild {
                flipped = true;
            } else {
                assert!(!flipped, "keep after rebuild at {n} requests");
            }
        }
    }
    let mut last = f64::MIN;
    for saved in (0..=40_000).step_by(2_500) {
        let c = RebuildCandidate {
            tokens_saved: saved,
            ..early_demotion()
        };
        let s = assess(Some(&cached()), warm(100_000), &c, 40)
            .saving
            .value()
            .unwrap();
        assert!(s >= last, "saving fell at {saved} tokens saved");
        last = s;
    }
}

#[test]
fn the_gate_only_skips_a_discretionary_rebuild_that_does_not_break_even_when_enforcing() {
    let skip = |mode, necessity, rec| gate(mode, necessity, rec).action == GateAction::Skipped;
    use CacheAware::{Advise, Enforce, Off};
    use Necessity::{Discretionary, RequiredToFit};
    use Recommendation::{Keep, Rebuild, Unknown};
    assert!(skip(Enforce, Discretionary, Keep));
    for rec in [Keep, Rebuild, Unknown] {
        assert!(!skip(Off, Discretionary, rec));
        assert!(
            !skip(Advise, Discretionary, rec),
            "advise never changes what is done"
        );
        // The context window outranks cost.
        assert!(!skip(Enforce, RequiredToFit, rec));
    }
    assert!(!skip(Enforce, Discretionary, Rebuild));
    // An unknown price never justifies skipping.
    let d = gate(Enforce, Discretionary, Unknown);
    assert_eq!(
        (d.action, d.reason),
        (GateAction::Rebuilt, GateReason::PriceUnknown)
    );
    let d = gate(Enforce, RequiredToFit, Keep);
    assert_eq!(d.reason, GateReason::RequiredToFit);
}

#[test]
fn the_record_carries_every_number_the_decision_used() {
    let c = early_demotion();
    let a = assess(Some(&cached()), warm(100_000), &c, 10);
    let d = gate(
        CacheAware::Advise,
        Necessity::Discretionary,
        a.recommendation,
    );
    let r = record(&c, &a, &d, warm(100_000), 10);
    assert_eq!(r.kind, RebuildKind::Demotion);
    assert_eq!(r.recommendation, Recommendation::Keep);
    assert_eq!(r.action, GateAction::Rebuilt);
    assert_eq!(r.reason, GateReason::Advisory);
    assert_eq!(
        (
            r.prompt_tokens,
            r.tokens_saved,
            r.first_changed_token,
            r.warm_tokens,
            r.remaining_requests
        ),
        (100_000, 10_000, 20_000, 100_000, 10)
    );
    assert_eq!(r.keep, a.keep);
    assert_eq!(r.saving, a.saving);
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["recommendation"], "keep");
    assert_eq!(json["action"], "rebuilt");
}

#[test]
fn the_remaining_requests_are_declared_or_derived_from_the_session_so_far() {
    // Declared: used as given.
    assert_eq!(expected_remaining_requests(40, Some(7), 50), 7);
    // Derived: a session that made k requests is expected to make about k more,
    // at least one, and never more than the cap.
    assert_eq!(expected_remaining_requests(0, None, 50), 1);
    assert_eq!(expected_remaining_requests(12, None, 50), 12);
    assert_eq!(expected_remaining_requests(400, None, 50), 50);
}
