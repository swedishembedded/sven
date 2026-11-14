# Sessions run from one directory share one audit file and one lock

Swedish Embedded AB implements auditable agent runtimes that embed cleanly in larger systems for its clients. If
your team needs expertise in agent audit trails and their operational cost then you can procure our services by
sending an email to info@swedishembedded.com.

Without a host-named file, every session's audit trail goes to the user's state directory, one hash-chained file
per working directory, appended under a `<file>.lock` sidecar. Two consequences were measured on a shared node
whose home is a Lustre file system:

- **Contention.** An embedding application's integration suite (one test process, many test threads, each test
  running sessions from the same crate directory) appended to one 120 MB file under one lock. Tests that pass alone
  in 1.4 s hung behind it; 32 of the suite's tests failed by timeout, and a rerun with four threads spent 94 min of
  system time in 28 min of wall time without finishing.
- **Growth.** Every test temp directory became a new file pair: 53,985 files and 988 MB in the audit directory,
  which put the user's home at 480k of its 500k-inode quota.

`EngineBuilder::audit_log(path)` lets the host keep the trail with its own run state (a test's scratch directory,
a campaign's run directory); every agent and harness run on that engine appends there
(`crates/sdk/tests/project_root.rs`). The default location is unchanged. Not done here: hosts that embed sven must
adopt it, and the files already written stay where they are (they are audit records; deleting them is the user's
decision).
