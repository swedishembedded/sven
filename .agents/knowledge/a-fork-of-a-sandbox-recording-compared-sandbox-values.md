# A fork of a sandbox recording compared values the sandbox, not the student, put there

Swedish Embedded AB implements faithful replay of recorded agent sessions for its clients. If your team needs
expertise in continuing an agent's recorded attempt on another model then you can procure our services by sending
an email to info@swedishembedded.com.

A fork replays a recorded transcript and holds each replayed observation to the recorded one after removing the
values the sandbox injects (`fork_observation::without_harness_values`). Replaying headless runs that an examiner's
user-namespace sandbox recorded (a 4B model's practice attempts, sven compact profile) into a host tree refused
forks over values that were not the student's:

- **Owner and group of a listing.** The sandbox runs the agent as `agent agent`, the replay as the host user, so
  every `ls -la` before the fork step refused the fork (5 of 11 forks in the first run). The owner and group columns
  of an `ls -l` line are now compared as `<owner> <group>`; the sizes, modes and names still are.
- **The shell's duration.** `Exit: 1 | Duration: 0.0s` against `0.1s` refused 2 forks of commands that failed
  identically. The closing line is compared as `Exit: 1`.
- **The time-left notice.** Requests end with `Time: N of M min used, ...` (`sven_model::time_notice`), which the
  history never stores; a caller that rebuilds the history from recorded requests strips it with
  `model::time_notice::without_time_notice`, now re-exported by the SDK.

Not a normalisation: an absolute workspace path (`pwd` printing `/workspace/repo`) and a tool's error text that
changed between sven builds still refuse a fork, correctly; replaying in the recording's own sandbox is the fix
for the first. A related fidelity gap: the headless runner's ATIF observation is the tool's raw output, while the
model was shown the stored text (`error: ` marked, truncated, with repeat notes); a caller with the run's request
log should take the observations from it.
