// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements gated self-improvement loops where a model
// update is adopted only on held-out evidence. If your team needs expertise
// in evaluation-gated fine-tuning, you can procure our services by sending
// an email to info@swedishembedded.com.

//! Training on the loop's own verified experience, with the gate outside
//! the trainer's optimism: an adapter is promoted only when the HELD-OUT
//! loss actually improved, and every attempt - adopted or rejected - leaves
//! both scores on record.
//!
//! The pipeline is brain's own trainer, called as a library: the dataset
//! pool is validated with the same parser training will use
//! (`brain::validate_chat_dataset_for`), packed by
//! `data::chat::prepare_chat_samples`, fine-tuned by `qwen3::finetune_from`
//! as a LoRA over the base checkpoint, and scored before and after with
//! `qwen3::eval::score_chat` on the same held-out samples. Nothing here is
//! a second trainer or a second evaluator.
//!
//! Promotion writes the adapter and a pointer file that names it; rejection
//! keeps the adapter for inspection but writes no pointer. Either way the
//! decision record holds both scores, so "the model got worse" is evidence,
//! not folklore.

use crate::learn;
use crate::store;
use data::chat::ChatSample;
use model::FitOpts;
use qwen3::finetune::{finetune_from, Mode};
use std::path::PathBuf;

/// Options for one training attempt.
#[derive(Clone, Debug)]
pub(crate) struct TrainOptions {
    /// Base checkpoint to fine-tune against (the model the agent serves).
    pub model_dir: PathBuf,
    /// Dataset file to train on; defaults to the accumulated pool.
    pub dataset: Option<PathBuf>,
    /// Training steps (small by default: the loop trains on its own
    /// verified experience, a few samples at a time).
    pub steps: u32,
    /// LoRA rank / alpha for the adapter.
    pub rank: u32,
    pub alpha: f32,
}

/// The held-out verdict for one training attempt.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scores {
    pub base_loss: f32,
    pub tuned_loss: f32,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Promoted,
    Rejected,
}

/// The gate: an adapter is adopted only when the held-out loss strictly
/// improved, and a non-finite score is a rejection, never an adoption -
/// "the evaluator failed" must not read as "the model improved".
/// The gate. A candidate is promoted only when its held-out loss strictly
/// improves on the base AND, when a standing champion measured on the same
/// pool and split (equal base loss - same data, same holdout), on the
/// champion too. Losses from different held-out sets do not compare, so an
/// unmatched champion never bounds a candidate. A candidate that beats the
/// base but not the champion is rejected: promoting it would make serving
/// worse than what it replaces.
pub(crate) fn decide(scores: &Scores, champion: Option<&Scores>) -> Decision {
    if !scores.base_loss.is_finite() || !scores.tuned_loss.is_finite() {
        return Decision::Rejected;
    }
    if scores.tuned_loss >= scores.base_loss {
        return Decision::Rejected;
    }
    if let Some(champ) = champion {
        if champ.base_loss == scores.base_loss
            && champ.base_loss.is_finite()
            && champ.tuned_loss.is_finite()
            && scores.tuned_loss >= champ.tuned_loss
        {
            return Decision::Rejected;
        }
    }
    Decision::Promoted
}

/// One held-out sample is the minimum honest evaluation; anything less has
/// nothing to hold out, and training on the evaluation set would make the
/// gate read training loss. The newest sample is held out: the pool is
/// appended in run order, so the most recent verified experience is the
/// one that must still generalize.
pub(crate) fn split(samples: &[ChatSample]) -> Option<(&[ChatSample], &[ChatSample])> {
    if samples.len() < 2 {
        return None;
    }
    // 10% held out, at least one: a single record's 20-odd token positions
    // cannot carry a promotion verdict on a pool of hundreds - its noise
    // would masquerade as improvement or regression. The newest records are
    // held out: the pool is appended in run order, so the most recent
    // verified experience is the one that must still generalize.
    let val = (samples.len() / 10).max(1);
    let (train, val) = samples.split_at(samples.len() - val);
    Some((train, val))
}

/// The smallest training row that can hold the longest example, rounded to
/// a power of two so a slightly longer future sample does not force a
/// re-architect. `fit` refuses a block smaller than an example.
fn block_size(longest_example: usize) -> u32 {
    let mut block = 64u32;
    while (block as usize) < longest_example {
        block *= 2;
    }
    block
}

fn opts(steps: u32, block: u32) -> FitOpts {
    FitOpts {
        steps,
        batch_size: 1,
        block_size: block,
        // Scaled-down defaults: a handful of steps over a handful of
        // samples wants a short schedule, or every step is still warming up
        // when the budget ends.
        lr: 3e-4,
        min_lr: 3e-5,
        warmup: steps / 5,
        decay_iters: steps,
        weight_decay: 0.1,
        grad_clip: 1.0,
        grad_accum: 1,
        eval_interval: 0,
        eval_batches: 0,
        seed: 1337,
        checkpoint_secs: 0,
        mask_before: None,
        mask_per_line: false,
        align_to_lines: false,
        // The gate is this module's own before/after scoring on the held-out
        // sample; the trainer's internal early stop would save a checkpoint
        // chosen by the TRAIN loss it sees, not the gate's evidence.
        patience: 0,
    }
}

/// One training attempt, end to end. Returns the decision and where the
/// decision record (and, on promotion, the adapter) lives.
pub(crate) fn run(options: &TrainOptions) -> anyhow::Result<(Decision, PathBuf)> {
    let model_dir = options.model_dir.clone();
    // brain's loader APIs take the checkpoint FILE; a caller may pass the
    // standard directory, so resolve once and score/train against the
    // checkpoint inside it.
    let weights = crate::provider::resolve_base(&model_dir)?;
    let pool = match &options.dataset {
        Some(path) => path.clone(),
        None => learn::pool_path(),
    };
    anyhow::ensure!(
        pool.exists(),
        "no dataset at {} - learn a verified run first",
        pool.display()
    );

    // The same parse training will run, before a device is claimed: a pool
    // that fails shape, encoding, or supervision dies here with the
    // offending record named.
    let summary =
        brain::validate_chat_dataset_for(&pool, &model_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let samples = learn::read_pool(&pool)?;
    let (train_samples, val_samples) = split(&samples).ok_or_else(|| {
        anyhow::anyhow!(
            "{} holds {} record(s); evaluation needs at least 2 so one can be held out",
            pool.display(),
            summary.records
        )
    })?;

    let tmpl = data::chat_template::ChatTemplate::from_model_dir(&model_dir)?;
    let tok = data::qwen_tokenizer::QwenBpe::from_file(
        model_dir
            .join("tokenizer.json")
            .to_str()
            .expect("model path is utf-8"),
    )
    .map_err(|e| anyhow::anyhow!("loading tokenizer from {model_dir:?}: {e}"))?;

    // The block must hold the longest example, or training silently
    // supervises nothing for it.
    let prepared = data::chat::prepare_chat_samples(
        train_samples,
        val_samples,
        &tok,
        &tmpl,
        tok.vocab_size(),
        &train_dir()?,
    )?;
    let block = block_size(prepared.longest_example);
    eprintln!(
        "train: {} record(s), longest example {} tokens, block {block}",
        summary.records, prepared.longest_example
    );

    eprintln!("train: scoring base against the held-out sample");
    // Both scores run at the same reduced weight tier: the gate compares
    // two numbers, and two numbers at different precisions do not compare.
    let dt = gpu_core::select::Dtype::F16;
    let base = qwen3::eval::score_chat_dt(
        weights.to_str().expect("model path is utf-8"),
        None,
        &tok,
        &tmpl,
        val_samples,
        block,
        dt,
    );

    let out = attempt_dir()?;
    let adapter = out.join("adapter.safetensors");
    let fit_opts = opts(options.steps, block);
    // finetune_from writes a full training checkpoint (adapter tensors on
    // top of the base), which the scorer cannot fold; the scoreable,
    // ModelCard-carrying adapter file comes from re-loading that checkpoint
    // with its adapters trainable and saving through qwen3::lora, the same
    // two-step shape `brain qwen3 lora-train` uses.
    eprintln!(
        "train: tuning {} step(s), lora rank {}",
        options.steps, options.rank
    );
    let full_ckpt = out.join("full.safetensors");
    finetune_from(
        weights.to_str().expect("model path is utf-8"),
        &train_dir()?,
        &fit_opts,
        &Mode::Lora {
            rank: options.rank,
            alpha: options.alpha,
        },
        full_ckpt.to_str().expect("checkpoint path is utf-8"),
        false,
    )?;

    let reader = checkpoint::weightio::WeightReader::open(
        full_ckpt.to_str().expect("checkpoint path is utf-8"),
    )
    .map_err(|e| anyhow::anyhow!("reopening trained checkpoint: {e}"))?;
    let cfg = qwen3::config::QwenConfig::from_json(&reader.config());
    let shard = qwen3::Shard::whole(cfg.n_layers as usize);
    let reloaded = qwen3::model::Qwen::new_shard(cfg, 1, block, &reader, true, shard);
    // The card identity names what produced the adapter and what it sits
    // on, so a promoted adapter is traceable to its own training evidence.
    let base_id = format!(
        "local/{}",
        model_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("base")
    );
    let card_id = format!("{base_id}:loop:experience:latest");
    qwen3::lora::save_adapter(
        adapter.to_str().expect("adapter path is utf-8"),
        &reloaded,
        &card_id,
        &base_id,
        None,
    )
    .map_err(|e| anyhow::anyhow!("saving LoRA adapter: {e}"))?;

    eprintln!("train: scoring the tuned adapter against the held-out sample");
    let tuned = qwen3::eval::score_chat_dt(
        weights.to_str().expect("model path is utf-8"),
        Some(adapter.to_str().expect("adapter path is utf-8")),
        &tok,
        &tmpl,
        val_samples,
        block,
        dt,
    );

    let scores = Scores {
        base_loss: base.loss,
        tuned_loss: tuned.loss,
    };
    // The standing champion's scores, from the pointer: beats-base is not
    // enough when a better adapter already serves - the gate must not let a
    // regression displace it. Only a champion scored on the same pool and
    // split bounds a candidate; `decide` checks that by base loss.
    let champion = read_champion_scores(&adapter_pointer());
    let decision = decide(&scores, champion.as_ref());

    let record = serde_json::json!({
        "dataset": pool,
        "records": summary.records,
        "block_size": block,
        "steps": options.steps,
        "rank": options.rank,
        "base": { "loss": base.loss, "token_accuracy": base.token_accuracy, "positions": base.positions },
        "tuned": { "loss": tuned.loss, "token_accuracy": tuned.token_accuracy, "positions": tuned.positions },
        "decision": match decision { Decision::Promoted => "promoted", Decision::Rejected => "rejected" },
        "adapter": adapter,
    });
    store::write_atomic(
        &out.join("decision.json"),
        &serde_json::to_string_pretty(&record)?,
    )?;

    apply_decision(
        &decision,
        &adapter_pointer(),
        &adapter,
        &model_dir,
        &scores,
        &out.join("decision.json"),
    )?;
    Ok((decision, out))
}

/// Applies the gate's verdict to durable state. Promotion repoints the
/// adapter pointer at the new adapter, so serving follows the promotion.
/// Rejection changes nothing: the pointer names the previous known-good
/// adapter, and a failed training attempt must not take it out of service -
/// "no pointer" means this attempt wrote none, never that the standing
/// promotion was torn down.
fn apply_decision(
    decision: &Decision,
    pointer: &std::path::Path,
    adapter: &std::path::Path,
    model_dir: &std::path::Path,
    scores: &Scores,
    decision_record: &std::path::Path,
) -> anyhow::Result<()> {
    if *decision != Decision::Promoted {
        return Ok(());
    }
    let record = serde_json::json!({
        "adapter": adapter,
        "model_dir": model_dir,
        "scores": { "base_loss": scores.base_loss, "tuned_loss": scores.tuned_loss },
        "decision_record": decision_record,
    });
    store::write_atomic(pointer, &serde_json::to_string_pretty(&record)?)
        .map_err(|e| anyhow::anyhow!("{}: {e}", pointer.display()))
}

/// One training attempt's own output directory, created exactly once.
fn attempt_dir() -> anyhow::Result<PathBuf> {
    let dir = store::state_root()
        .join("train")
        .join(store::new_id_with_prefix("train"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn train_dir() -> anyhow::Result<PathBuf> {
    let dir = store::state_root().join("train").join("prepared");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Where serving reads the currently promoted adapter from, if any.
pub(crate) fn adapter_pointer() -> PathBuf {
    store::state_root().join("adapter.json")
}

/// The champion's recorded gate scores, when a pointer is in place. A
/// missing or malformed pointer is no champion - the base-only rule
/// applies - never an error: a rejected first attempt leaves no pointer,
/// and training must still work from there.
fn read_champion_scores(pointer: &std::path::Path) -> Option<Scores> {
    let text = std::fs::read_to_string(pointer).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Scores {
        base_loss: value.get("scores")?.get("base_loss")?.as_f64()? as f32,
        tuned_loss: value.get("scores")?.get("tuned_loss")?.as_f64()? as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_attempt_leaves_the_promoted_adapter_in_service() {
        let _guard = crate::store::ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("loop-train-gate-{}", std::process::id()));
        std::env::set_var("SVEN_LOOP_STATE", &root);
        let pointer = store::state_root().join("adapter.json");
        std::fs::create_dir_all(pointer.parent().unwrap()).unwrap();
        let standing = serde_json::json!({ "adapter": "/previous/good-adapter.safetensors" });
        std::fs::write(&pointer, serde_json::to_string_pretty(&standing).unwrap()).unwrap();

        // A rejection writes nothing and tears nothing down: the standing
        // promotion stays servable.
        apply_decision(
            &Decision::Rejected,
            &pointer,
            std::path::Path::new("/this/attempt/adapter.safetensors"),
            std::path::Path::new("/base"),
            &Scores {
                base_loss: 1.0,
                tuned_loss: 1.2,
            },
            std::path::Path::new("/this/attempt/decision.json"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&pointer).unwrap(),
            serde_json::to_string_pretty(&standing).unwrap(),
            "a failed training must not remove the known-good promotion"
        );

        // A promotion repoints the pointer at the new adapter.
        let adapter = std::path::Path::new("/this/attempt/adapter.safetensors");
        apply_decision(
            &Decision::Promoted,
            &pointer,
            adapter,
            std::path::Path::new("/base"),
            &Scores {
                base_loss: 1.2,
                tuned_loss: 1.0,
            },
            std::path::Path::new("/this/attempt/decision.json"),
        )
        .unwrap();
        let promoted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pointer).unwrap()).unwrap();
        assert_eq!(promoted["adapter"], adapter.display().to_string());
        assert_eq!(promoted["scores"]["tuned_loss"], 1.0);

        std::env::remove_var("SVEN_LOOP_STATE");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn promotion_requires_a_strictly_lower_heldout_loss() {
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 2.0,
                    tuned_loss: 1.9
                },
                None,
            ),
            Decision::Promoted
        );
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 1.9,
                    tuned_loss: 1.9
                },
                None,
            ),
            Decision::Rejected
        );
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 1.9,
                    tuned_loss: 2.0
                },
                None,
            ),
            Decision::Rejected
        );
        // A failed evaluation (NaN) is never an improvement.
        assert_eq!(
            decide(
                &Scores {
                    base_loss: f32::NAN,
                    tuned_loss: 1.0
                },
                None,
            ),
            Decision::Rejected
        );
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 2.0,
                    tuned_loss: f32::NAN
                },
                None,
            ),
            Decision::Rejected
        );
    }

    /// A candidate that beats the base but not the standing champion must
    /// not displace it: the gate exists so serving never gets worse, and
    /// the champion's held-out loss is the bar when the two attempts
    /// measured on the same pool, split, and base (equal base loss).
    #[test]
    fn a_candidate_worse_than_the_champion_is_rejected() {
        // Same base loss: comparable. Champion tuned 0.165; candidate 0.9
        // beats the base (4.8) but not the champion - rejected.
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 4.8,
                    tuned_loss: 0.9
                },
                Some(&Scores {
                    base_loss: 4.8,
                    tuned_loss: 0.165
                }),
            ),
            Decision::Rejected
        );
        // A candidate better than both is promoted.
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 4.8,
                    tuned_loss: 0.1
                },
                Some(&Scores {
                    base_loss: 4.8,
                    tuned_loss: 0.165
                }),
            ),
            Decision::Promoted
        );
        // Champion scores from a DIFFERENT pool (base loss differs) do not
        // bound the candidate: losses measured on different held-out data
        // do not compare, so the base-only rule applies.
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 4.8,
                    tuned_loss: 0.9
                },
                Some(&Scores {
                    base_loss: 3.0,
                    tuned_loss: 0.165
                }),
            ),
            Decision::Promoted
        );
        // No champion recorded: base-only rule.
        assert_eq!(
            decide(
                &Scores {
                    base_loss: 4.8,
                    tuned_loss: 0.9
                },
                None,
            ),
            Decision::Promoted
        );
    }

    #[test]
    fn the_split_holds_the_newest_sample_out_and_needs_two() {
        let one = vec![ChatSample::default()];
        assert!(split(&one).is_none(), "one record has nothing to hold out");

        let three = vec![
            ChatSample::default(),
            ChatSample::default(),
            ChatSample::default(),
        ];
        let (train, val) = split(&three).unwrap();
        assert_eq!(train.len(), 2);
        // Pointer identity: the held-out sample is the LAST one, the
        // newest verified experience.
        assert!(std::ptr::eq(val.as_ptr(), &three[2]));
    }

    /// One held-out sample cannot gate a large pool: 21 token positions of
    /// one record decided a promotion that the train split contradicted.
    /// The holdout grows with the pool (10%, minimum one) so a promotion
    /// verdict rests on more than one record's noise.
    #[test]
    fn the_holdout_grows_with_the_pool() {
        let mut big: Vec<ChatSample> = (0..150).map(|_| ChatSample::default()).collect();
        let (train, val) = split(&big).unwrap();
        assert_eq!(val.len(), 15, "10% of 150");
        assert_eq!(train.len(), 135);
        // Newest records are the held-out ones.
        assert!(std::ptr::eq(val.as_ptr(), &big[135]));

        let mut small: Vec<ChatSample> = (0..9).map(|_| ChatSample::default()).collect();
        let (train, val) = split(&small).unwrap();
        assert_eq!(val.len(), 1, "at least one, even at 10% < 1");
        assert_eq!(train.len(), 8);
    }

    #[test]
    fn the_block_grows_to_hold_the_longest_example() {
        assert_eq!(block_size(10), 64);
        assert_eq!(block_size(64), 64);
        assert_eq!(block_size(65), 128);
        assert_eq!(block_size(1000), 1024);
    }
}
