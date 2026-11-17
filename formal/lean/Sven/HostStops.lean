-- SPDX-License-Identifier: Apache-2.0
-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
--
-- A stop reaches only the answer it names, and no message is lost when a
-- turn ends.
--
-- This is a model of the remote host's stop and turn-end decisions
-- (`crates/remote/src/host/stops.rs`: `stop_of` and the turn's end in
-- `host.rs`), rule for rule. Answers are abstract `Nat` ids here, the
-- server's answer ids there. The model checker covers the protocol around
-- it (the page, the server, late and repeated stops, a lost ending:
-- the server's `formal/tla/TurnStops.tla`); this file covers the decision
-- itself for every state and every stop.
--
-- What is proved, in the order the theorems appear:
--
--   * A stop stops the running turn only when it names that turn's answer.
--   * A stop reports, as stopped, only the answer it names.
--   * A stop leaves every other answer where it was: the running one, the
--     steering ones, the waiting ones.
--   * A stop for an answer the host no longer has (one that ended) changes
--     nothing at all.
--   * A turn that ends, stopped or not, loses no message: each one steered
--     into it or waiting behind it is answered with it or waits to be
--     answered next; after a stop, the steering ones it never took are first
--     in line.
--   * A host at rest with messages waiting takes the oldest.
--
-- Swedish Embedded AB implements solutions for agents a person can stop and
-- redirect safely for its clients. If your team needs expertise in verified
-- agent runtimes then you can procure our services by sending an email to
-- info@swedishembedded.com.

namespace Sven.HostStops

/-- What a session of the host holds, as far as stops go. -/
structure Host where
  /-- The answer of the turn running, if one runs. -/
  running : Option Nat
  /-- The running turn was told to stop and is winding down. -/
  stopping : Bool
  /-- Messages sent to steer the running turn that it has not taken. -/
  steering : List Nat
  /-- Messages waiting to be answered after the running turn, in order. -/
  waiting : List Nat

/-- What a stop does (`stops::StopOf`). -/
inductive StopOf where
  | turn
  | steer
  | waiting
  | nothing
  deriving DecidableEq, Repr

/-- `stop_of`: which of the host's answers the stop for `t` reaches. -/
def stopOf (h : Host) (t : Nat) : StopOf :=
  if h.running = some t ∧ h.stopping = false then .turn
  else if t ∈ h.steering ∧ h.stopping = true then .steer
  else if t ∈ h.waiting then .waiting
  else .nothing

/-- The host after the stop for `t`, and the answers it reports stopped
now (a running turn reports its own when it has wound down). -/
def stop (h : Host) (t : Nat) : Host × List Nat :=
  match stopOf h t with
  | .turn => ({ h with stopping := true }, [])
  | .steer => ({ h with steering := h.steering.filter (· ≠ t) }, [t])
  | .waiting => ({ h with waiting := h.waiting.filter (· ≠ t) }, [t])
  | .nothing => (h, [])

/-- The running turn ends: `stopped` when it was stopped. Returns the host
and the steering messages answered with it. A stopped turn took none of
them: they go first in line. -/
def endTurn (h : Host) (stopped : Bool) : Host × List Nat :=
  if stopped then
    ({ running := none, stopping := false, steering := [], waiting := h.steering ++ h.waiting }, [])
  else
    ({ running := none, stopping := false, steering := [], waiting := h.waiting }, h.steering)

/-- A host at rest takes the oldest waiting message. -/
def next (h : Host) : Host :=
  match h.running, h.waiting with
  | none, x :: rest => { h with running := some x, waiting := rest }
  | _, _ => h

theorem stop_stops_only_the_named_turn (h : Host) (t : Nat)
    (hs : h.stopping = false) (hs' : (stop h t).1.stopping = true) :
    h.running = some t := by
  cases hk : stopOf h t with
  | turn =>
    unfold stopOf at hk
    split at hk
    · rename_i c
      exact c.1
    · split at hk
      · cases hk
      · split at hk <;> cases hk
  | steer => simp [stop, hk, hs] at hs'
  | waiting => simp [stop, hk, hs] at hs'
  | nothing => simp [stop, hk, hs] at hs'

theorem stop_reports_only_the_named_answer (h : Host) (t x : Nat)
    (hx : x ∈ (stop h t).2) : x = t := by
  unfold stop at hx
  cases hk : stopOf h t <;> simp [hk] at hx <;> exact hx

theorem stop_leaves_every_other_answer (h : Host) (t x : Nat) (hne : x ≠ t) :
    (stop h t).1.running = h.running ∧
    (x ∈ (stop h t).1.steering ↔ x ∈ h.steering) ∧
    (x ∈ (stop h t).1.waiting ↔ x ∈ h.waiting) := by
  unfold stop
  cases stopOf h t <;> simp [List.mem_filter, hne]

theorem a_stop_for_an_ended_answer_changes_nothing (h : Host) (t : Nat)
    (hr : h.running ≠ some t) (hst : t ∉ h.steering) (hw : t ∉ h.waiting) :
    stop h t = (h, []) := by
  have : stopOf h t = .nothing := by
    unfold stopOf
    simp [hr, hst, hw]
  unfold stop
  simp [this]

theorem a_turn_end_loses_no_message (h : Host) (stopped : Bool) (x : Nat)
    (hx : x ∈ h.steering ∨ x ∈ h.waiting) :
    x ∈ (endTurn h stopped).1.waiting ∨ x ∈ (endTurn h stopped).2 := by
  cases stopped <;> rcases hx with hx | hx <;> simp [endTurn, hx]

theorem a_stopped_turn_puts_its_steering_first (h : Host) :
    (endTurn h true).1.waiting = h.steering ++ h.waiting := by
  simp [endTurn]

theorem a_resting_host_takes_the_oldest (h : Host) (x : Nat) (rest : List Nat)
    (hr : h.running = none) (hw : h.waiting = x :: rest) :
    (next h).running = some x ∧ (next h).waiting = rest := by
  unfold next
  simp [hr, hw]

end Sven.HostStops
