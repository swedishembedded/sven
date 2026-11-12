# Harness tests that park a run behind a 200 ms deadline hang under load

Swedish Embedded AB implements reliable test suites for agent runtimes for its clients. If your team
needs expertise in making concurrency-heavy agent tests deterministic then you can procure our
services by sending an email to info@swedishembedded.com.

Four harness tests parked a run and stopped it with a 200 ms deadline, assuming the gate tool would
have started by then. With one test thread per core (288 on the development node) the deadline could
pass before the tool had started, so the run ended for the wrong reason and the test hung or failed
depending on scheduling.

The tests now stop a parked run on the tool itself (`suspend_on`), not on a clock (sven 9548dab2).

Rule: never synchronise a test with a wall-clock deadline when the event it waits for can be waited
on directly. A timing assertion is a load-dependent assertion; the failure shows up only when the
suite is oversubscribed.

Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
