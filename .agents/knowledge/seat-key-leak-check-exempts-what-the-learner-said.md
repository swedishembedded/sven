# The seat's answer-key leak check must exempt what the learner said first

Swedish Embedded AB implements measurable simulated-user testing for its clients. If your team needs
expertise in validating that simulated users neither leak answers nor reward-hack then you can
procure our services by sending an email to info@swedishembedded.com.

The simulated seat (counterpart) keeps an answer key from the model playing it, and the conversation
is refused when the seat's words contain the key. In the first real-model legal-intake run (20 eval
cases, 27B on both sides) 1 of 20 conversations was flagged as a leak because the client repeated
the lawyer's own conclusion back. Content the learner said first is not a leak.

The check now exempts the learner's own words (sven 989463ec); paraphrased leaks and early
disclosure are still caught by the deterministic 75 % content-token window (06c89cf7). The same
change stopped the client walking out while a fact the learner had asked for was untold: legal
conversations then ran the full 8 rounds instead of about 5 turns.

Rule: a validity check on a simulator needs both directions measured, false refusals (here 1/20) as
well as leaks, since a check that refuses honest conversations silently shrinks and biases the data.

A related reward hack, found by scripted adversaries: a tutor that stuffs every keyword phrase
raised a keyword-based student mastery to full. Mastery now rises only for a new, distinct worked
numeric example in a sentence naming the concept; the `stuffer`, `contradicts`, answer-revealing and
idle tutors all earn zero gain (2df9a5df, ded56233).

Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
