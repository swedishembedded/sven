-- SPDX-License-Identifier: Apache-2.0
-- Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
--
-- The axioms each headline theorem rests on, printed at build time and read
-- by `formal/run-lean.sh`: anything but Lean's own three axioms (`sorryAx`
-- above all) fails the suite.
import Sven.Goal
import Sven.HostStops

open Sven.Goal

#print axioms only_a_pass_completes_a_checked_goal
#print axioms verified_only_by_a_pass
#print axioms no_verdict_never_completes
#print axioms a_stopped_goal_takes_no_turn
#print axioms continue_only_active_within_budget
#print axioms check_continue_only_active_within_budget
#print axioms every_goal_ends
#print axioms blocked_only_after_a_streak
#print axioms the_same_blocker_three_times_blocks
#print axioms a_report_completes_an_unchecked_goal

open Sven.HostStops

#print axioms stop_stops_only_the_named_turn
#print axioms stop_reports_only_the_named_answer
#print axioms stop_leaves_every_other_answer
#print axioms a_stop_for_an_ended_answer_changes_nothing
#print axioms a_turn_end_loses_no_message
#print axioms a_stopped_turn_puts_its_steering_first
#print axioms a_resting_host_takes_the_oldest
