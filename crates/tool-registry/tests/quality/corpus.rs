// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Seeded catalogs of overlapping tools and the queries that mean one of them.
//!
//! **What the generator assumes.** A tool is a (domain, object, verb) triple.
//! Its name, its one-sentence summary and its parameters always use the plain
//! words. The longer description mentions a synonym of the object, a synonym
//! of the verb and a gloss of the domain only some of the time (a probability
//! drawn per catalog), as hand-written tool descriptions do. A paraphrased
//! query is made of synonyms, an indirect one of a gloss of the domain. A query
//! whose words appear nowhere in the tool's text cannot be answered by any
//! lexical ranker; the probabilities set how often that happens.
//!
//! **Splits.** Tuning and validation share no catalog seed, no domain, object
//! or verb, and no query template family (see `lexicon` and `Query::family`).

use sven_tool_registry::{ToolCapability, ToolDescriptor};

use super::lexicon::{domains, verbs, Domain, Verb};
use super::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Split {
    Tuning,
    Validation,
}

impl Split {
    /// Query template families are numbered across splits: the tuning split
    /// owns 0-2 of each kind, the validation split 3-5.
    fn first_family(self) -> usize {
        match self {
            Self::Tuning => 0,
            Self::Validation => 3,
        }
    }

    fn first_seed(self) -> u64 {
        match self {
            Self::Tuning => 1,
            Self::Validation => 1001,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QueryKind {
    /// Uses the words of the tool's name.
    Exact,
    /// Uses synonyms of them.
    Paraphrase,
    /// Describes the tool's domain without naming it.
    Indirect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub description: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: String,
    /// The first sentence of the description: what a descriptor shows.
    pub summary: String,
    pub description: String,
    pub params: Vec<Param>,
    pub modes: Vec<String>,
    pub capability: ToolCapability,
    pub mcp: bool,
}

impl ToolSpec {
    pub fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name.clone(),
            summary: self.summary.clone(),
            capability: self.capability,
            mcp: self.mcp,
            description: self.description.clone(),
            parameter_descriptions: {
                let mut named: Vec<_> = self
                    .params
                    .iter()
                    .map(|p| (&p.name, &p.description))
                    .collect();
                named.sort();
                named.into_iter().map(|(_, text)| text.clone()).collect()
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    pub kind: QueryKind,
    pub family: usize,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Catalog {
    pub seed: u64,
    /// The mean of the three probabilities with which the description states
    /// a synonym of the object, a synonym of the verb and a gloss of the
    /// domain: how much evidence the catalog's text holds for paraphrases.
    pub evidence: f64,
    /// In the order a registry would hand them over: shuffled.
    pub tools: Vec<ToolSpec>,
    pub queries: Vec<Query>,
}

impl Catalog {
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools.iter().map(ToolSpec::descriptor).collect()
    }
}

/// How many catalogs a split has, how big they are and how many queries of
/// each kind each one gets.
pub struct Plan {
    pub catalogs: usize,
    pub sizes: &'static [usize],
    pub queries_per_kind: usize,
}

impl Plan {
    /// 6 catalogs of 40-288 tools and 180 queries per split: runs in `cargo
    /// test` in well under a second.
    pub fn standard() -> Self {
        Self {
            catalogs: 6,
            sizes: &[40, 80, 144, 200, 288, 96],
            queries_per_kind: 10,
        }
    }

    /// 24 catalogs and 1,440 queries per split.
    pub fn sweep() -> Self {
        Self {
            catalogs: 24,
            sizes: &[80, 144, 200, 288],
            queries_per_kind: 20,
        }
    }
}

pub fn catalogs(split: Split, plan: &Plan) -> Vec<Catalog> {
    (0..plan.catalogs)
        .map(|i| {
            let size = plan.sizes[i % plan.sizes.len()];
            generate(
                split,
                split.first_seed() + i as u64,
                size,
                plan.queries_per_kind,
            )
        })
        .collect()
}

/// How the words of a catalog are written, fixed by its seed.
struct Style {
    naming: u64,
    summary: u64,
    server_prefix: bool,
    native: bool,
    object_synonym: f64,
    verb_synonym: f64,
    gloss: f64,
}

type Combo = (&'static Domain, usize, &'static Verb);

fn generate(split: Split, seed: u64, size: usize, per_kind: usize) -> Catalog {
    let mut rng = Rng::new(seed, "catalog");
    let style = Style {
        naming: seed % 4,
        summary: (seed / 4) % 3,
        server_prefix: seed.is_multiple_of(2),
        native: seed.is_multiple_of(3),
        object_synonym: 0.2 * rng.below(5) as f64,
        verb_synonym: 0.2 * rng.below(5) as f64,
        gloss: 0.2 * rng.below(5) as f64,
    };
    let mut combos: Vec<Combo> = Vec::new();
    for domain in domains(split) {
        for object in 0..domain.objects.len() {
            for verb in verbs(split) {
                combos.push((domain, object, verb));
            }
        }
    }
    rng.shuffle(&mut combos);
    combos.truncate(size);
    let tools: Vec<ToolSpec> = combos.iter().map(|c| tool(&mut rng, &style, *c)).collect();

    let mut order: Vec<usize> = (0..tools.len()).collect();
    Rng::new(seed, "targets").shuffle(&mut order);
    let mut queries = Vec::new();
    let mut words = Rng::new(seed, "queries");
    for (n, kind) in [QueryKind::Exact, QueryKind::Paraphrase, QueryKind::Indirect]
        .into_iter()
        .enumerate()
    {
        for (i, &at) in order[n * per_kind..(n + 1) * per_kind].iter().enumerate() {
            let family = split.first_family() + (i + seed as usize) % 3;
            queries.push(query(&mut words, kind, family, combos[at], &tools[at].name));
        }
    }
    let evidence = (style.object_synonym + style.verb_synonym + style.gloss) / 3.0;
    Catalog {
        seed,
        evidence,
        tools,
        queries,
    }
}

fn tool(rng: &mut Rng, style: &Style, (domain, object, verb): Combo) -> ToolSpec {
    let (noun, synonym) = domain.objects[object];
    let clause = verb.clause.replace("{}", noun);
    let summary = match style.summary {
        0 => format!("{clause} in the {} system.", domain.name),
        1 => format!("{clause} ({}).", domain.name),
        _ => format!("{}: {}.", domain.name, lower_first(&clause)),
    };

    let mut description = summary.clone();
    if rng.chance(style.object_synonym) {
        description.push_str(&format!(" Also known as working with a {synonym}."));
    }
    let said: Vec<&str> = verb
        .synonyms
        .iter()
        .copied()
        .filter(|_| rng.chance(style.verb_synonym))
        .collect();
    if !said.is_empty() {
        description.push_str(&format!(" Use it to {} a record.", said.join(" or ")));
    }
    if rng.chance(style.gloss) {
        description.push_str(&format!(" Part of the toolset for {}.", domain.gloss));
    }
    description.push_str(" Takes the identifier of the record and returns the updated record.");

    let identifier = if rng.chance(style.object_synonym) {
        synonym
    } else {
        noun
    };
    let mut params = vec![Param {
        name: format!("{noun}_id"),
        description: format!("Identifier of the {identifier}."),
        values: Vec::new(),
    }];
    params.push(match verb.word {
        "list" | "find" => enumerated(
            "status",
            "Only records in this state.",
            &["open", "closed", "pending"],
        ),
        "export" => enumerated(
            "format",
            "File format of the copy.",
            &["csv", "json", "pdf"],
        ),
        "cancel" | "delete" | "approve" | "archive" => enumerated(
            "reason",
            &format!("Why, for the {} audit log.", domain.name),
            &[],
        ),
        _ => enumerated("fields", &format!("Field values of the {noun}."), &[]),
    });

    let capability = match (style.native, verb.read_only, verb.destructive) {
        (false, ..) => ToolCapability::NetworkAccess,
        (true, true, _) => ToolCapability::ReadFile,
        (true, _, true) => ToolCapability::DeleteFile,
        (true, ..) => ToolCapability::WriteFile,
    };
    let modes = if verb.read_only {
        vec!["agent", "plan", "research"]
    } else {
        vec!["agent"]
    };
    ToolSpec {
        name: name(style, domain.name, noun, verb.word),
        summary,
        description,
        params,
        modes: modes.into_iter().map(String::from).collect(),
        capability,
        mcp: !style.native,
    }
}

fn enumerated(name: &str, description: &str, values: &[&str]) -> Param {
    Param {
        name: name.into(),
        description: description.into(),
        values: values.iter().map(|v| (*v).to_string()).collect(),
    }
}

fn name(style: &Style, domain: &str, object: &str, verb: &str) -> String {
    let base = match style.naming {
        0 => format!("{domain}_{object}_{verb}"),
        1 => format!("{domain}-{object}-{verb}"),
        2 => format!("{domain}{}{}", capitalise(object), capitalise(verb)),
        _ => format!("{verb}_{object}_{domain}"),
    };
    if style.server_prefix {
        format!("srv-{base}")
    } else {
        base
    }
}

fn query(
    rng: &mut Rng,
    kind: QueryKind,
    family: usize,
    (domain, object, verb): Combo,
    target: &str,
) -> Query {
    let (noun, synonym) = domain.objects[object];
    let variant = family % 3;
    let synonym_verb = verb.synonyms[rng.below(3)];
    let (d, g) = (domain.name, domain.gloss);
    let text = match (kind, family >= 3, variant) {
        (QueryKind::Exact, false, 0) => format!("{} {noun} {d}", verb.word),
        (QueryKind::Exact, false, 1) => format!("{} {noun}s, {d}", verb.gerund),
        (QueryKind::Exact, false, _) => format!("{noun} {}", verb.word),
        (QueryKind::Exact, true, 0) => format!("tool for {} {noun} ({d})", verb.gerund),
        (QueryKind::Exact, true, 1) => format!("{d}: {noun} / {}", verb.third_person),
        (QueryKind::Exact, true, _) => format!("how do I {} {noun} for {d}?", verb.word),
        (QueryKind::Paraphrase, false, 0) => format!("I need to {synonym_verb} a {synonym} in {d}"),
        (QueryKind::Paraphrase, false, 1) => format!("{synonym_verb} {synonym} ({d})"),
        (QueryKind::Paraphrase, false, _) => format!("{synonym}: {synonym_verb}, {d} system"),
        (QueryKind::Paraphrase, true, 0) => {
            format!("how can I {synonym_verb} the {synonym} for {d}?")
        }
        (QueryKind::Paraphrase, true, 1) => format!("want to {synonym_verb} {synonym}s, {d}"),
        (QueryKind::Paraphrase, true, _) => format!("{d} task: {synonym} to {synonym_verb}"),
        (QueryKind::Indirect, false, 0) => format!("{synonym_verb} {synonym} for {g}"),
        (QueryKind::Indirect, false, 1) => {
            format!("in the area of {g}, {synonym_verb} a {synonym}")
        }
        (QueryKind::Indirect, false, _) => format!("{synonym}: {synonym_verb} ({g})"),
        (QueryKind::Indirect, true, 0) => format!("when {g}, how to {synonym_verb} {synonym}"),
        (QueryKind::Indirect, true, 1) => format!("I work on {g}; {synonym_verb} the {synonym}"),
        (QueryKind::Indirect, true, _) => format!("{g} - {synonym} - {synonym_verb}"),
    };
    Query {
        text,
        kind,
        family,
        target: target.to_string(),
    }
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_lowercase().chain(chars).collect())
        .unwrap_or_default()
}
