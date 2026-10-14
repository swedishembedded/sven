// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool retrieval for
// large agent tool sets then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The words the generated catalogs are made of. The two splits use disjoint
//! domains, objects and verbs; only filler words are common to both.

use super::corpus::Split;

pub struct Domain {
    pub name: &'static str,
    /// What the domain is about, said without its name.
    pub gloss: &'static str,
    /// Object and a synonym for it.
    pub objects: [(&'static str, &'static str); 4],
}

pub struct Verb {
    pub word: &'static str,
    pub third_person: &'static str,
    pub gerund: &'static str,
    pub synonyms: [&'static str; 3],
    /// A summary clause; `{}` is the object.
    pub clause: &'static str,
    pub read_only: bool,
    pub destructive: bool,
}

const fn d(
    name: &'static str,
    gloss: &'static str,
    objects: [(&'static str, &'static str); 4],
) -> Domain {
    Domain {
        name,
        gloss,
        objects,
    }
}

const TUNING_DOMAINS: [Domain; 12] = [
    d(
        "shipping",
        "moving parcels between places",
        [
            ("parcel", "package"),
            ("route", "itinerary"),
            ("carrier", "courier"),
            ("manifest", "cargo list"),
        ],
    ),
    d(
        "ledger",
        "recording what a company owns and owes",
        [
            ("entry", "posting"),
            ("account", "books"),
            ("budget", "spending plan"),
            ("receipt", "proof of purchase"),
        ],
    ),
    d(
        "recruiting",
        "hiring new staff",
        [
            ("candidate", "applicant"),
            ("vacancy", "open position"),
            ("interview", "screening call"),
            ("offer", "job proposal"),
        ],
    ),
    d(
        "fleet",
        "managing company vehicles",
        [
            ("vehicle", "car"),
            ("driver", "chauffeur"),
            ("trip", "journey"),
            ("fuelcard", "petrol card"),
        ],
    ),
    d(
        "cinema",
        "showing films to audiences",
        [
            ("film", "movie"),
            ("screening", "showtime"),
            ("seat", "chair"),
            ("voucher", "free pass"),
        ],
    ),
    d(
        "clinic",
        "treating patients",
        [
            ("patient", "client"),
            ("prescription", "medicine order"),
            ("visit", "consultation"),
            ("diagnosis", "finding"),
        ],
    ),
    d(
        "forum",
        "online discussion boards",
        [
            ("thread", "topic"),
            ("post", "message"),
            ("moderator", "warden"),
            ("badge", "award"),
        ],
    ),
    d(
        "kitchen",
        "preparing meals in a restaurant",
        [
            ("recipe", "formula"),
            ("dish", "course"),
            ("order", "request"),
            ("menu", "bill of fare"),
        ],
    ),
    d(
        "survey",
        "asking people questions",
        [
            ("questionnaire", "form"),
            ("response", "answer"),
            ("panel", "respondent group"),
            ("quota", "target count"),
        ],
    ),
    d(
        "garden",
        "growing plants",
        [
            ("plant", "seedling"),
            ("bed", "plot"),
            ("harvest", "crop yield"),
            ("sprinkler", "watering system"),
        ],
    ),
    d(
        "telecom",
        "phone and network service",
        [
            ("subscriber", "line holder"),
            ("tariff", "rate plan"),
            ("sim", "chip card"),
            ("outage", "service break"),
        ],
    ),
    d(
        "museum",
        "exhibiting collections to visitors",
        [
            ("artifact", "relic"),
            ("exhibit", "display"),
            ("ticket", "entry pass"),
            ("curator", "keeper"),
        ],
    ),
];

const VALIDATION_DOMAINS: [Domain; 12] = [
    d(
        "banking",
        "keeping customer money safe",
        [
            ("deposit", "payment in"),
            ("loan", "credit line"),
            ("mortgage", "home financing"),
            ("statement", "balance sheet"),
        ],
    ),
    d(
        "hotel",
        "lodging guests overnight",
        [
            ("booking", "stay"),
            ("suite", "premium room"),
            ("guest", "lodger"),
            ("minibar", "room fridge"),
        ],
    ),
    d(
        "school",
        "teaching pupils",
        [
            ("lesson", "class period"),
            ("pupil", "student"),
            ("grade", "mark"),
            ("timetable", "weekly plan"),
        ],
    ),
    d(
        "farm",
        "raising crops and animals",
        [
            ("field", "pasture"),
            ("tractor", "farm machine"),
            ("herd", "livestock group"),
            ("silo", "grain store"),
        ],
    ),
    d(
        "insurance",
        "covering people against loss",
        [
            ("policy", "cover contract"),
            ("claim", "damage report"),
            ("premium", "cover fee"),
            ("adjuster", "loss assessor"),
        ],
    ),
    d(
        "airline",
        "flying passengers between cities",
        [
            ("flight", "air service"),
            ("crew", "cabin staff"),
            ("boarding", "gate entry"),
            ("baggage", "luggage"),
        ],
    ),
    d(
        "parking",
        "leaving cars in lots",
        [
            ("permit", "parking pass"),
            ("garage", "car park"),
            ("meter", "pay station"),
            ("violation", "parking fine"),
        ],
    ),
    d(
        "gym",
        "exercising in a fitness club",
        [
            ("membership", "club pass"),
            ("trainer", "coach"),
            ("workout", "exercise plan"),
            ("locker", "storage cabinet"),
        ],
    ),
    d(
        "pharmacy",
        "dispensing medicines",
        [
            ("remedy", "drug"),
            ("dosage", "dose amount"),
            ("refill", "repeat supply"),
            ("batch", "production lot"),
        ],
    ),
    d(
        "theatre",
        "staging live plays",
        [
            ("play", "stage show"),
            ("rehearsal", "practice run"),
            ("costume", "stage outfit"),
            ("understudy", "stand-in actor"),
        ],
    ),
    d(
        "laundry",
        "cleaning clothes for customers",
        [
            ("garment", "clothing item"),
            ("cycle", "wash program"),
            ("stain", "spot mark"),
            ("pickup", "collection round"),
        ],
    ),
    d(
        "auction",
        "selling items to the highest bidder",
        [
            ("lot", "sale item"),
            ("bid", "offer price"),
            ("bidder", "buyer"),
            ("catalogue", "sale listing"),
        ],
    ),
];

/// A verb from its forms (base, third person, gerund), synonyms, summary
/// clause and effect (read only, destructive).
const fn v(
    forms: (&'static str, &'static str, &'static str),
    synonyms: [&'static str; 3],
    clause: &'static str,
    effect: (bool, bool),
) -> Verb {
    Verb {
        word: forms.0,
        third_person: forms.1,
        gerund: forms.2,
        synonyms,
        clause,
        read_only: effect.0,
        destructive: effect.1,
    }
}

const TUNING_VERBS: [Verb; 6] = [
    v(
        ("create", "creates", "creating"),
        ["make", "add", "set up"],
        "Creates a new {}",
        (false, false),
    ),
    v(
        ("list", "lists", "listing"),
        ["show", "enumerate", "display"],
        "Lists every {}",
        (true, false),
    ),
    v(
        ("cancel", "cancels", "cancelling"),
        ["call off", "abort", "revoke"],
        "Cancels an existing {}",
        (false, true),
    ),
    v(
        ("export", "exports", "exporting"),
        ["download", "dump", "extract"],
        "Exports a copy of a {}",
        (true, false),
    ),
    v(
        ("archive", "archives", "archiving"),
        ["put away", "shelve", "store"],
        "Archives a {}",
        (false, false),
    ),
    v(
        ("update", "updates", "updating"),
        ["change", "modify", "edit"],
        "Updates the fields of a {}",
        (false, false),
    ),
];

const VALIDATION_VERBS: [Verb; 6] = [
    v(
        ("delete", "deletes", "deleting"),
        ["remove", "erase", "discard"],
        "Deletes a {} permanently",
        (false, true),
    ),
    v(
        ("find", "finds", "finding"),
        ["look up", "locate", "search for"],
        "Finds a {} by its text",
        (true, false),
    ),
    v(
        ("merge", "merges", "merging"),
        ["combine", "join", "unite"],
        "Merges two {} records into one",
        (false, false),
    ),
    v(
        ("approve", "approves", "approving"),
        ["sign off", "accept", "authorize"],
        "Approves a pending {}",
        (false, false),
    ),
    v(
        ("duplicate", "duplicates", "duplicating"),
        ["copy", "clone", "replicate"],
        "Duplicates an existing {}",
        (false, false),
    ),
    v(
        ("schedule", "schedules", "scheduling"),
        ["plan", "book", "arrange"],
        "Schedules a {} for later",
        (false, false),
    ),
];

pub fn domains(split: Split) -> &'static [Domain] {
    match split {
        Split::Tuning => &TUNING_DOMAINS,
        Split::Validation => &VALIDATION_DOMAINS,
    }
}

pub fn verbs(split: Split) -> &'static [Verb] {
    match split {
        Split::Tuning => &TUNING_VERBS,
        Split::Validation => &VALIDATION_VERBS,
    }
}
