-- SPDX-License-Identifier: Apache-2.0
-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
--
-- The goal policy decides honestly, for every goal and every turn.
--
-- This is a model of `crates/machines/src/machines/goal.rs`: `afterTurn`,
-- `afterCheck` and `decide` below are its `after_turn`, `after_check` and
-- `decide`, rule for rule and in the same order (blocker identity is an
-- abstract `Nat` key here, the normalised words there). `formal/tla/Goal.tla`
-- checks the protocol around the policy for every interleaving at small
-- bounds; this file checks the policy itself for every value - every budget,
-- every count, every sequence of turns.
--
-- What is proved, in the order the theorems appear:
--
--   * A goal with a check is never completed by the model's report: only a
--     passing verdict completes it, and only a passing verdict marks it
--     verified.
--   * A check without a verdict never completes a goal.
--   * A goal that is not active takes no further turn and is not changed.
--   * Only an active goal continues, and a continuing goal has turns left:
--     no goal turn starts past the budget.
--   * Every decision to continue uses up a turn, so a goal stops after at
--     most `maxTurns` turns, whatever each turn reports.
--   * A goal is blocked only after the same blocker on `blockedAfter`
--     consecutive turns, or `idleAfter` turns in a row with no action.
--
-- Swedish Embedded AB implements solutions for agents that keep working
-- toward an objective and stop for the right reasons. If your team needs
-- expertise in verified agent runtimes then you can procure our services by
-- sending an email to info@swedishembedded.com.

namespace Sven.Goal

/-- `BLOCKED_AFTER` in `sven_vocab::goal`. -/
def blockedAfter : Nat := 3
/-- `IDLE_AFTER` in `sven_vocab::goal`. -/
def idleAfter : Nat := 3

inductive Status where
  | active | paused | blocked | completed | budgetExhausted
  deriving DecidableEq, Repr

/-- The parts of `sven_vocab::goal::Goal` the policy reads and writes. -/
structure Goal where
  status : Status
  turnsUsed : Nat
  maxTurns : Nat
  hasCheck : Bool
  verified : Bool
  blockedTurns : Nat
  blockerKey : Option Nat
  idleTurns : Nat
  deriving Repr

/-- What the model reported in a turn (`GoalReport`), if anything. -/
inductive Report where
  | none
  | complete
  | blocked (key : Nat)
  deriving DecidableEq, Repr

/-- `TurnRecord`: whether the turn acted, and its last report. -/
structure Turn where
  acted : Bool
  report : Report

inductive Verdict where
  | passed | failed | unknown
  deriving DecidableEq, Repr

inductive Next where
  | check | continue | stop
  deriving DecidableEq, Repr

def Report.isSome : Report → Bool
  | .none => false
  | _ => true

/-- Rules 2 to 6 of `goal.rs`, once the check (if any) has not completed the
goal. With a check, a report of completion counts for nothing: the check
decides. -/
def decide (g : Goal) (t : Turn) : Goal × Next :=
  if !g.hasCheck && t.report == .complete then
    ({ g with status := .completed, verified := false }, .stop)
  else if blockedAfter ≤ g.blockedTurns then
    ({ g with status := .blocked }, .stop)
  else if idleAfter ≤ g.idleTurns then
    ({ g with status := .blocked }, .stop)
  else if g.maxTurns ≤ g.turnsUsed then
    ({ g with status := .budgetExhausted }, .stop)
  else
    (g, .continue)

/-- `after_turn`: count the turn and what it showed, then check, stop or go
on. -/
def afterTurn (g : Goal) (t : Turn) : Goal × Next :=
  if g.status ≠ .active then (g, .stop) else
  let idle := if t.acted || t.report.isSome then 0 else g.idleTurns + 1
  let (streak, key) :=
    match t.report with
    | .blocked k =>
        if g.blockerKey = some k then (g.blockedTurns + 1, some k) else (1, some k)
    | _ => (0, none)
  let g' := { g with turnsUsed := g.turnsUsed + 1, idleTurns := idle,
                     blockedTurns := streak, blockerKey := key }
  if g'.hasCheck then (g', .check) else decide g' t

/-- `after_check`: a pass completes the goal whatever its status; no verdict
pauses an active goal; a failure goes on as the other rules say. -/
def afterCheck (g : Goal) (v : Verdict) (t : Turn) : Goal × Next :=
  match v with
  | .passed => ({ g with status := .completed, verified := true }, .stop)
  | .unknown =>
      if g.status = .active then ({ g with status := .paused }, .stop) else (g, .stop)
  | .failed =>
      if g.status = .active then decide g t else (g, .stop)

/-! ## Completion is earned -/

/-- With a check, `decide` never completes a goal. -/
theorem decide_with_check_never_completes (g : Goal) (t : Turn)
    (hc : g.hasCheck = true) (hs : g.status ≠ .completed) :
    (decide g t).1.status ≠ .completed := by
  simp only [decide]
  repeat' split
  all_goals simp_all

/-- A turn of a goal with a check never completes it: it goes to the check. -/
theorem after_turn_with_check_goes_to_the_check (g : Goal) (t : Turn)
    (ha : g.status = .active) (hc : g.hasCheck = true) :
    (afterTurn g t).2 = .check ∧ (afterTurn g t).1.status = .active := by
  simp only [afterTurn]
  repeat' split
  all_goals simp_all

/-- Only a passing verdict completes a goal that has a check. -/
theorem only_a_pass_completes_a_checked_goal (g : Goal) (v : Verdict) (t : Turn)
    (hc : g.hasCheck = true) (hs : g.status ≠ .completed)
    (done : (afterCheck g v t).1.status = .completed) : v = .passed := by
  cases v with
  | passed => rfl
  | unknown =>
      revert done; simp only [afterCheck]
      repeat' split
      all_goals simp_all
  | failed =>
      revert done; simp only [afterCheck]
      split
      · exact fun d => absurd d (decide_with_check_never_completes g t hc hs)
      · simp_all

/-- A goal is marked verified only by a passing verdict. -/
theorem verified_only_by_a_pass (g : Goal) (v : Verdict) (t : Turn)
    (hv : g.verified = false) (h : (afterCheck g v t).1.verified = true) :
    v = .passed := by
  cases v with
  | passed => rfl
  | unknown =>
      revert h; simp only [afterCheck]
      repeat' split
      all_goals simp_all
  | failed =>
      revert h; simp only [afterCheck, decide]
      repeat' split
      all_goals simp_all

/-- A check without a verdict never completes a goal: an active one pauses. -/
theorem no_verdict_never_completes (g : Goal) (t : Turn) (hs : g.status ≠ .completed) :
    (afterCheck g .unknown t).1.status ≠ .completed := by
  simp only [afterCheck]
  repeat' split
  all_goals simp_all

/-! ## Stopped goals stay stopped -/

theorem a_stopped_goal_takes_no_turn (g : Goal) (t : Turn) (h : g.status ≠ .active) :
    afterTurn g t = (g, .stop) := by
  simp [afterTurn, h]

/-- `decide` continues only the goal it was given, with turns left. -/
theorem decide_continue (g : Goal) (t : Turn) (h : (decide g t).2 = .continue) :
    decide g t = (g, .continue) ∧ g.turnsUsed < g.maxTurns := by
  revert h; simp only [decide]
  repeat' split
  all_goals (try simp_all) <;> omega

/-- Only an active goal continues, and only with turns left. -/
theorem continue_only_active_within_budget (g : Goal) (t : Turn)
    (h : (afterTurn g t).2 = .continue) :
    (afterTurn g t).1.status = .active ∧
    (afterTurn g t).1.turnsUsed = g.turnsUsed + 1 ∧
    (afterTurn g t).1.turnsUsed < g.maxTurns := by
  revert h; simp only [afterTurn, decide]
  repeat' split
  all_goals (try simp_all) <;> omega

/-- After a failed check, too, a goal continues only active and with turns
left. -/
theorem check_continue_only_active_within_budget (g : Goal) (v : Verdict) (t : Turn)
    (h : (afterCheck g v t).2 = .continue) :
    (afterCheck g v t).1.status = .active ∧ g.turnsUsed < g.maxTurns := by
  revert h; cases v <;> simp only [afterCheck, decide]
  all_goals (repeat' split)
  all_goals (try simp_all) <;> omega

/-! ## Every goal ends -/

/-- Feed a goal one turn after another until it stops; the number of turns
it took. -/
def run : Goal → List Turn → Nat
  | _, [] => 0
  | g, t :: ts =>
      match afterTurn g t with
      | (g', .continue) => 1 + run g' ts
      | _ => 1

/-- A turn never changes the budget it is counted against. -/
theorem after_turn_keeps_the_budget (g : Goal) (t : Turn) :
    (afterTurn g t).1.maxTurns = g.maxTurns := by
  simp only [afterTurn, decide]
  repeat' split
  all_goals simp_all

/-- However its turns go, a goal stops within the turns it has left. -/
theorem every_goal_ends (g : Goal) (ts : List Turn) :
    run g ts ≤ g.maxTurns - g.turnsUsed + 1 := by
  induction ts generalizing g with
  | nil => simp [run]
  | cons t ts ih =>
      simp only [run]
      split
      · rename_i g' heq
        have hc : (afterTurn g t).2 = .continue := by rw [heq]
        have hb := continue_only_active_within_budget g t hc
        have hm := after_turn_keeps_the_budget g t
        rw [heq] at hb hm
        have := ih g'
        simp only at hb hm
        omega
      · omega

/-! ## Blocked for a reason -/

/-- A turn blocks a goal only when the same blocker was reported on
`blockedAfter` turns in a row, or no action was taken for `idleAfter`. -/
theorem blocked_only_after_a_streak (g : Goal) (t : Turn) (ha : g.status = .active)
    (h : (afterTurn g t).1.status = .blocked) :
    blockedAfter ≤ (afterTurn g t).1.blockedTurns ∨ idleAfter ≤ (afterTurn g t).1.idleTurns := by
  revert h; simp only [afterTurn, decide]
  repeat' split
  all_goals simp_all

/-- One report of a blocker does not block a goal that was not reporting one. -/
theorem one_report_does_not_block (g : Goal) (k : Nat) (ha : g.status = .active)
    (hn : g.blockerKey = none) (hm : g.turnsUsed + 1 < g.maxTurns) (hc : g.hasCheck = false) :
    (afterTurn g ⟨true, .blocked k⟩).2 = .continue := by
  simp only [afterTurn, decide, Report.isSome]
  repeat' split
  all_goals simp_all [blockedAfter, idleAfter]
  all_goals omega

/-- Three reports of the same blocker in a row block it. -/
theorem the_same_blocker_three_times_blocks (g : Goal) (k : Nat) (ha : g.status = .active)
    (hk : g.blockerKey = some k) (hb : g.blockedTurns = 2) (hc : g.hasCheck = false) :
    (afterTurn g ⟨true, .blocked k⟩).1.status = .blocked := by
  simp [afterTurn, ha, hk, hb, hc, decide, blockedAfter]

/-- Without a check, the model's report completes the goal, unverified. -/
theorem a_report_completes_an_unchecked_goal (g : Goal) (ha : g.status = .active)
    (hc : g.hasCheck = false) :
    (afterTurn g ⟨true, .complete⟩).1.status = .completed ∧
    (afterTurn g ⟨true, .complete⟩).1.verified = false := by
  simp [afterTurn, ha, hc, decide]

end Sven.Goal
