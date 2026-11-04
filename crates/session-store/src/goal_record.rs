// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective across restarts for its clients. If your team needs expertise in
// resumable agent sessions then you can procure our services by sending an
// email to info@swedishembedded.com.
//! A session's standing goal, kept with its saved trajectory.
//!
//! The goal is session state outside the conversation: it is saved beside
//! the steps, under its own key of `Trajectory.extra`, so that a session
//! opened again can put it back (`GoalCommand::Restore`, which brings an
//! active goal back paused). A file written before goals existed, or with a
//! goal this build cannot read, simply has none.

use atif::Trajectory;
use sven_vocab::goal::Goal;

/// The key the goal is kept under in `Trajectory.extra`.
pub const GOAL_EXTRA_KEY: &str = "goal";

/// Keeps `goal` with `trajectory`, or removes the one kept (`None`).
pub fn write_goal(trajectory: &mut Trajectory, goal: Option<&Goal>) {
    let extra = trajectory
        .extra
        .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(map) = extra.as_object_mut() else {
        return;
    };
    match goal.and_then(|g| serde_json::to_value(g).ok()) {
        Some(value) => {
            map.insert(GOAL_EXTRA_KEY.to_string(), value);
        }
        None => {
            map.remove(GOAL_EXTRA_KEY);
        }
    }
}

/// The goal kept with `trajectory`, if there is one this build can read.
#[must_use]
pub fn read_goal(trajectory: &Trajectory) -> Option<Goal> {
    trajectory
        .extra
        .as_ref()
        .and_then(|extra| extra.get(GOAL_EXTRA_KEY))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trajectory() -> Trajectory {
        Trajectory::new("ATIF-v1.7", atif::AgentProfile::new("sven", "test"))
    }

    #[test]
    fn a_kept_goal_comes_back_and_a_cleared_one_does_not() {
        let mut trajectory = trajectory();
        trajectory.extra = Some(serde_json::json!({ "other": 1 }));
        let goal = Goal::new("goal-2".into(), "ship it".into(), 20, None);
        write_goal(&mut trajectory, Some(&goal));
        assert_eq!(read_goal(&trajectory), Some(goal));
        write_goal(&mut trajectory, None);
        assert_eq!(read_goal(&trajectory), None);
        assert_eq!(trajectory.extra.unwrap()["other"], 1, "other keys are kept");
    }

    #[test]
    fn a_file_without_a_goal_has_none() {
        assert_eq!(read_goal(&trajectory()), None);
    }
}
