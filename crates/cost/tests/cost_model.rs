// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The cost model's spec: every term visible, an unknown price stays unknown,
//! and the orderings a decision relies on hold over a grid of prices and sizes.

use sven_cost::{
    cheapest, estimate, Amount, CacheState, CostTerms, Placement, PromptShape, Recovery, StepInputs,
};
use sven_model_catalog::{CachePricing, Pricing};
use sven_vocab::ReportedUsage;

fn cached(input: f64, output: f64, read: f64, write: f64, ttl: u32) -> Pricing {
    Pricing {
        input_per_mtok: input,
        output_per_mtok: output,
        cache: Some(CachePricing {
            read_per_mtok: read,
            write_per_mtok: write,
            ttl_secs: Some(ttl),
        }),
    }
}

fn plain(input: f64, output: f64) -> Pricing {
    Pricing {
        input_per_mtok: input,
        output_per_mtok: output,
        cache: None,
    }
}

fn step(prefix: u32, new: u32, output: u32, placement: Placement) -> StepInputs {
    StepInputs {
        shape: PromptShape {
            prefix_tokens: prefix,
            new_tokens: new,
            output_tokens: output,
        },
        placement,
        recovery: Recovery::none(),
    }
}

fn total(terms: &CostTerms) -> f64 {
    terms.total().value().expect("known")
}

/// A deterministic generator, so the grids are the same on every run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: u32) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % u64::from(bound)) as u32
    }
    fn price(&mut self) -> f64 {
        f64::from(self.next(2_000)) / 100.0
    }
    /// A well-formed cached price: a read never costs more than a write.
    fn pricing(&mut self) -> Pricing {
        let (a, b) = (self.price(), self.price());
        let (read, write) = if a <= b { (a, b) } else { (b, a) };
        cached(self.price(), self.price(), read, write, 300)
    }
}

#[test]
fn every_term_is_visible_and_they_add_up() {
    let p = cached(3.0, 15.0, 0.3, 3.75, 300);
    let t = estimate(
        Some(&p),
        &step(
            100_000,
            2_000,
            500,
            Placement::Continue(CacheState {
                warm_tokens: 60_000,
            }),
        ),
    );
    // 60k warm tokens read, 40k cold tokens written, 2k new tokens at the
    // input rate, 500 generated.
    assert_eq!(t.cached_read, Amount::Known(60_000.0 * 0.3 / 1e6));
    assert_eq!(t.cache_write, Amount::Known(40_000.0 * 3.75 / 1e6));
    assert_eq!(t.input_uncached, Amount::Known(2_000.0 * 3.0 / 1e6));
    assert_eq!(t.output, Amount::Known(500.0 * 15.0 / 1e6));
    assert_eq!(t.handoff, Amount::Known(0.0));
    assert_eq!(t.recovery, Amount::Known(0.0));
    let sum = [
        t.input_uncached,
        t.cached_read,
        t.cache_write,
        t.output,
        t.handoff,
        t.recovery,
    ]
    .iter()
    .map(|a| a.value().unwrap())
    .sum::<f64>();
    assert!((total(&t) - sum).abs() < 1e-12);
}

#[test]
fn a_provider_without_a_cache_charges_the_whole_prompt_as_input() {
    let p = plain(2.0, 8.0);
    let t = estimate(
        Some(&p),
        &step(
            10_000,
            1_000,
            100,
            Placement::Continue(CacheState {
                warm_tokens: 10_000,
            }),
        ),
    );
    assert_eq!(t.cached_read, Amount::Known(0.0));
    assert_eq!(t.cache_write, Amount::Known(0.0));
    assert_eq!(t.input_uncached, Amount::Known(11_000.0 * 2.0 / 1e6));
}

#[test]
fn an_unknown_price_makes_every_term_and_the_total_unknown() {
    let t = estimate(None, &step(1_000, 100, 10, Placement::Switch));
    assert_eq!(t.total(), Amount::Unknown);
    assert_eq!(t.input_uncached, Amount::Unknown);
    assert_eq!(t.output, Amount::Unknown);
    assert_eq!(t.handoff, Amount::Unknown);
}

#[test]
fn unknown_serialises_as_the_word_unknown_and_known_as_a_number() {
    assert_eq!(
        serde_json::to_string(&Amount::Unknown).unwrap(),
        "\"unknown\""
    );
    assert_eq!(serde_json::to_string(&Amount::Known(1.5)).unwrap(), "1.5");
    let back: Amount = serde_json::from_str("\"unknown\"").unwrap();
    assert_eq!(back, Amount::Unknown);
    let back: Amount = serde_json::from_str("0.25").unwrap();
    assert_eq!(back, Amount::Known(0.25));
}

#[test]
fn switching_models_pays_the_whole_prefix_as_handoff_and_reads_nothing() {
    let p = cached(3.0, 15.0, 0.3, 3.75, 300);
    let t = estimate(Some(&p), &step(50_000, 1_000, 200, Placement::Switch));
    assert_eq!(t.handoff, Amount::Known(50_000.0 * 3.75 / 1e6));
    assert_eq!(t.cached_read, Amount::Known(0.0));
    assert_eq!(t.cache_write, Amount::Known(0.0));
}

#[test]
fn the_cache_expires_with_its_ttl_and_an_unknown_idle_time_is_not_assumed_warm() {
    assert_eq!(
        CacheState::expected(10_000, Some(300), Some(299)).warm_tokens,
        10_000
    );
    assert_eq!(
        CacheState::expected(10_000, Some(300), Some(300)).warm_tokens,
        10_000
    );
    assert_eq!(
        CacheState::expected(10_000, Some(300), Some(301)).warm_tokens,
        0
    );
    assert_eq!(CacheState::expected(10_000, Some(300), None).warm_tokens, 0);
    assert_eq!(CacheState::expected(10_000, None, Some(1)).warm_tokens, 0);
}

#[test]
fn what_the_last_request_cached_is_what_stays_warm_inside_the_ttl() {
    let last = ReportedUsage {
        input: 200,
        output: 50,
        cache_read: 8_000,
        cache_write: 1_800,
    };
    assert_eq!(
        CacheState::after(&last, Some(300), Some(10)).warm_tokens,
        9_800
    );
    assert_eq!(
        CacheState::after(&last, Some(300), Some(900)).warm_tokens,
        0
    );
    assert_eq!(CacheState::after(&last, Some(300), None).warm_tokens, 0);
}

#[test]
fn a_failure_probability_charges_the_expected_rerun_and_zero_charges_nothing() {
    let p = plain(1.0, 1.0);
    let mut s = step(1_000, 0, 0, Placement::Switch);
    s.recovery = Recovery {
        failure_probability: 0.25,
        rerun: Amount::Known(4.0),
    };
    assert_eq!(estimate(Some(&p), &s).recovery, Amount::Known(1.0));
    s.recovery = Recovery {
        failure_probability: 0.0,
        rerun: Amount::Unknown,
    };
    assert_eq!(estimate(Some(&p), &s).recovery, Amount::Known(0.0));
    s.recovery = Recovery {
        failure_probability: 0.1,
        rerun: Amount::Unknown,
    };
    assert_eq!(estimate(Some(&p), &s).recovery, Amount::Unknown);
}

#[test]
fn cost_is_monotone_in_every_token_count() {
    let mut rng = Lcg(7);
    for _ in 0..400 {
        let p = rng.pricing();
        let (prefix, new, out) = (rng.next(200_000), rng.next(20_000), rng.next(5_000));
        let warm = rng.next(prefix + 1);
        for placement in [
            Placement::Continue(CacheState { warm_tokens: warm }),
            Placement::Switch,
        ] {
            let base = total(&estimate(Some(&p), &step(prefix, new, out, placement)));
            for (dp, dn, dout) in [(1_000, 0, 0), (0, 1_000, 0), (0, 0, 1_000)] {
                let more = total(&estimate(
                    Some(&p),
                    &step(prefix + dp, new + dn, out + dout, placement),
                ));
                assert!(more >= base, "{p:?} {placement:?}: {more} < {base}");
            }
        }
    }
}

#[test]
fn a_cache_hit_never_costs_more_than_a_miss() {
    let mut rng = Lcg(11);
    for _ in 0..400 {
        let p = rng.pricing();
        let (prefix, new, out) = (rng.next(200_000), rng.next(20_000), rng.next(5_000));
        let warm = rng.next(prefix + 1);
        let hit = total(&estimate(
            Some(&p),
            &step(
                prefix,
                new,
                out,
                Placement::Continue(CacheState { warm_tokens: warm }),
            ),
        ));
        let miss = total(&estimate(
            Some(&p),
            &step(
                prefix,
                new,
                out,
                Placement::Continue(CacheState { warm_tokens: 0 }),
            ),
        ));
        assert!(hit <= miss + 1e-12, "{p:?}: hit {hit} > miss {miss}");
    }
}

#[test]
fn handoff_grows_with_the_context_and_a_switch_never_beats_staying_warm() {
    let mut rng = Lcg(13);
    for _ in 0..400 {
        let p = rng.pricing();
        let prefix = rng.next(200_000);
        let handoff = |n: u32| {
            estimate(Some(&p), &step(n, 0, 0, Placement::Switch))
                .handoff
                .value()
                .unwrap()
        };
        assert!(handoff(prefix + 1_000) >= handoff(prefix));
        let stay = total(&estimate(
            Some(&p),
            &step(
                prefix,
                0,
                0,
                Placement::Continue(CacheState {
                    warm_tokens: prefix,
                }),
            ),
        ));
        assert!(total(&estimate(Some(&p), &step(prefix, 0, 0, Placement::Switch))) >= stay);
    }
}

#[test]
fn an_unknown_price_never_wins_and_nothing_is_chosen_when_all_are_unknown() {
    let amounts = [
        Amount::Unknown,
        Amount::Known(5.0),
        Amount::Unknown,
        Amount::Known(2.0),
        Amount::Known(9.0),
    ];
    assert_eq!(cheapest(&amounts), Some(3));
    assert_eq!(cheapest(&[Amount::Unknown, Amount::Unknown]), None);
    assert_eq!(cheapest(&[]), None);
    // A free model is a known price; a missing one is not free.
    assert_eq!(cheapest(&[Amount::Unknown, Amount::Known(0.0)]), Some(1));
    // The first of equal prices wins, so a decision is stable.
    assert_eq!(cheapest(&[Amount::Known(1.0), Amount::Known(1.0)]), Some(0));
}
