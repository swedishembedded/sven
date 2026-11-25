// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that record exactly what a model
// sampled for its clients. If your team needs expertise in training-grade
// trajectories then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The checks a sampled response must pass when the turn asked the server for
//! more than the sampled tokens: the most probable tokens per position and the
//! support a truncated sampler drew from. A response that was asked and did not
//! answer fails the turn; it is never recorded short or empty.

use sven_vocab::Sampled;

/// Fails a response that was asked for the sampler's support (at most `n` ids per position) and did not return a
/// consistent one: the support must be there, of the size asked, cover every generated token, and hold the token
/// drawn at each position whose support is not the whole row. A trainer replaying the draw would otherwise read a
/// short or wrong field as the behaviour policy.
pub(crate) fn check_sampler_support(sampled: &Sampled, n: u32) -> anyhow::Result<()> {
    let positions = sampled.token_ids.len();
    let Some(support) = &sampled.sampler_support else {
        anyhow::bail!(
            "the model was asked for the sampler support (at most {n} ids per generated token) and returned none; a server that cannot return it must be refused, not recorded empty"
        );
    };
    if support.cap != n {
        anyhow::bail!(
            "the model was asked for a sampler support of {n} and answered with a cap of {}",
            support.cap
        );
    }
    support.check(positions).map_err(|why| {
        anyhow::anyhow!("the sampler support of the response is inconsistent: {why}")
    })?;
    for (position, token) in sampled.token_ids.iter().enumerate() {
        let ids = support.at(position);
        if !ids.is_empty() && !ids.contains(token) {
            anyhow::bail!(
                "the sampler support at position {position} does not hold the generated token {token}"
            );
        }
    }
    Ok(())
}

/// Fails a response that was asked for the `k` most probable tokens per position and did not return them: every
/// generated token must have a non-empty list of at most `k` candidates. An empty or short field would be read by a
/// trainer as "no alternatives", so the turn fails instead.
pub(crate) fn check_top_logprobs(sampled: &Sampled, k: u32) -> anyhow::Result<()> {
    let positions = sampled.token_ids.len();
    let rows_ok = |rows: &[usize]| rows.iter().all(|&n| n > 0 && n <= k as usize);
    let ids: Vec<usize> = sampled.top_token_ids.iter().map(Vec::len).collect();
    let logprobs: Vec<usize> = sampled.top_logprobs.iter().map(Vec::len).collect();
    if positions == 0 || ids.len() != positions || ids != logprobs || !rows_ok(&ids) {
        anyhow::bail!(
            "the model was asked for the top {k} log-probabilities of each generated token and returned {} position(s) of candidates for {positions} generated token(s); a server that cannot return them must be refused, not recorded empty",
            ids.len()
        );
    }
    Ok(())
}
