// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements local-inference serving paths for coding
// agents. If your team needs expertise in GPU inference performance, you can
// procure our services by sending an email to info@swedishembedded.com.

//! Measures the local serving engine's prefill and decode rates against a
//! real checkpoint, the two numbers every serving timeout is set from.
//!
//! ```text
//! cargo run -p sample-agent-loop --example prefill_bench -- \
//!     <checkpoint-file> <tokenizer.json> [n-tokens] [tier f32|f16]
//! ```

use checkpoint::weightio::WeightReader;
use data::tokenizer::Tokenizer;
use gpu_core::select::Dtype;
use qwen3::config::QwenConfig;
use qwen3::model::{PrefillInput, Qwen};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 2,
        "usage: prefill_bench <checkpoint-file> <tokenizer.json> [n-tokens] [tier f32|f16]"
    );
    let ckpt = &args[0];
    let tokenizer_path = &args[1];
    let n: usize = match args.get(2) {
        Some(s) => s.parse()?,
        None => 4096,
    };
    let tier = match args.get(3).map(String::as_str) {
        Some("f16") => Dtype::F16,
        _ => Dtype::F32,
    };
    let ctx: u32 = (n + 512).next_power_of_two() as u32;

    let reader = WeightReader::open(ckpt).map_err(|e| anyhow::anyhow!("{ckpt}: {e}"))?;
    let cfg = QwenConfig::from_json(&reader.config());
    let shard = qwen3::Shard::whole(cfg.n_layers as usize);
    let t0 = std::time::Instant::now();
    let model = Qwen::new_shard_dt_decode(cfg, ctx, &reader, shard, tier);
    println!(
        "load: {} ({tier:?}), linears landed {:?}",
        t0.elapsed().as_secs_f32(),
        model.linear_dtype()
    );

    let tok = data::qwen_tokenizer::QwenBpe::from_file(tokenizer_path)
        .map_err(|e| anyhow::anyhow!("{tokenizer_path}: {e}"))?;
    // A prompt of realistic agent proportions: prose plus tool-schema-shaped
    // JSON, tokenized once and truncated to n.
    let prose = "You are a coding agent working in a git repository. \
                 Inspect the workspace, patch the failing function, and run the test suite. \
                 When you finish, report the changed files and the check results. ";
    let schema = r#"{"name":"write","description":"write a file","parameters":{"path":"str","content":"str"}}"#;
    let mut text = String::new();
    while tok.encode(&text).len() < n {
        text.push_str(prose);
        text.push_str(schema);
    }
    let mut ids = tok.encode(&text);
    ids.truncate(n);
    println!("prompt: {} tokens", ids.len());

    // Prefill in the serving engine's own chunk size, timing each chunk.
    let mut total = std::time::Duration::ZERO;
    for (i, chunk) in ids.chunks(512).enumerate() {
        let t = std::time::Instant::now();
        let inputs = chunk
            .iter()
            .map(|&t| PrefillInput::Token(t))
            .collect::<Vec<_>>();
        let _hidden = model.prefill(&inputs);
        let dt = t.elapsed();
        total += dt;
        println!(
            "prefill chunk {}: {} tokens in {:.3}s ({:.0} tok/s, chunk cum {:?})",
            i,
            chunk.len(),
            dt.as_secs_f32(),
            chunk.len() as f32 / dt.as_secs_f32(),
            total
        );
    }
    let rate = ids.len() as f32 / total.as_secs_f32();
    println!(
        "prefill total: {} tokens in {:.1}s = {:.0} tok/s",
        ids.len(),
        total.as_secs_f32(),
        rate
    );

    // Decode rate: step the same number of tokens.
    let steps = 32;
    let t = std::time::Instant::now();
    for _ in 0..steps {
        model.step(ids[0]);
    }
    let dt = t.elapsed();
    println!(
        "decode: {} tokens in {:.3}s = {:.1} tok/s",
        steps,
        dt.as_secs_f32(),
        steps as f32 / dt.as_secs_f32()
    );
    Ok(())
}
