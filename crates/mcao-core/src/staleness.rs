//! Pure view logic for bearings staleness and task merge ordering.
//!
//! Provides cloud run state staleness tracking, terminal state classification,
//! and merged display ordering across local and cloud targets.

/// Time in seconds after which an observation is considered stale.
pub const STALE_AFTER_SECS: u64 = 90;

/// Freshness classification of a cloud observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// The observation is reachable and was updated within [`STALE_AFTER_SECS`] seconds.
    Fresh,
    /// The observation is reachable but its last update is older than [`STALE_AFTER_SECS`] seconds.
    Stale,
    /// The cloud state could not be fetched.
    Unreachable,
}

/// Observation of a cloud run state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudObservation {
    /// Last observed state string.
    pub last_state: String,
    /// Timestamp in seconds when the observation was last updated.
    pub last_update_secs: u64,
    /// Whether the fetch succeeded.
    pub fetch_ok: bool,
}

/// Evaluates the freshness of a cloud observation relative to the current timestamp.
///
/// Returns [`Freshness::Unreachable`] when `!fetch_ok`.
/// Returns [`Freshness::Stale`] when `now_secs.saturating_sub(last_update_secs) > STALE_AFTER_SECS`.
/// Otherwise returns [`Freshness::Fresh`] (including future timestamps and exact threshold).
pub fn freshness(obs: &CloudObservation, now_secs: u64) -> Freshness {
    if !obs.fetch_ok {
        Freshness::Unreachable
    } else if now_secs.saturating_sub(obs.last_update_secs) > STALE_AFTER_SECS {
        Freshness::Stale
    } else {
        Freshness::Fresh
    }
}

/// Determines whether the given state string represents a terminal execution state.
///
/// Exactly `"succeeded"`, `"failed"`, `"timed_out"`, and `"cancelled"` (case-sensitive)
/// are considered terminal.
pub fn is_terminal_state(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "timed_out" | "cancelled")
}

/// Formats the observation state string for display according to its freshness.
///
/// Returns the state as-is if fresh, appends `" (stale)"` if stale,
/// or `" (stale, unreachable)"` if unreachable.
pub fn display_state(obs: &CloudObservation, now_secs: u64) -> String {
    match freshness(obs, now_secs) {
        Freshness::Fresh => obs.last_state.clone(),
        Freshness::Stale => format!("{} (stale)", obs.last_state),
        Freshness::Unreachable => format!("{} (stale, unreachable)", obs.last_state),
    }
}

/// A task entry for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Identifier of the task, unique per target.
    pub id: String,
    /// Creation timestamp in seconds.
    pub created_secs: u64,
    /// Target execution environment (e.g., local or cloud).
    pub target: String,
    /// Task state string.
    pub state: String,
}

/// Merges local and cloud task rows, sorted by creation timestamp ascending with tie-breaking on task ID.
///
/// The sort is stable. Because IDs are unique per target, identical IDs across local and cloud
/// are both retained.
pub fn merge_rows(local: Vec<Row>, cloud: Vec<Row>) -> Vec<Row> {
    let mut merged = local;
    merged.extend(cloud);
    merged.sort_by(|a, b| {
        a.created_secs
            .cmp(&b.created_secs)
            .then_with(|| a.id.cmp(&b.id))
    });
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_at_boundary_seconds() {
        let obs = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 100,
            fetch_ok: true,
        };

        // 89 seconds: strictly less than threshold -> Fresh
        assert_eq!(freshness(&obs, 189), Freshness::Fresh);

        // 90 seconds: exactly at threshold -> Fresh
        assert_eq!(freshness(&obs, 190), Freshness::Fresh);

        // 91 seconds: strictly greater than threshold -> Stale
        assert_eq!(freshness(&obs, 191), Freshness::Stale);
    }

    #[test]
    fn freshness_future_timestamp() {
        let obs = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 200,
            fetch_ok: true,
        };

        // Future update timestamp relative to now (100 < 200) -> Fresh
        assert_eq!(freshness(&obs, 100), Freshness::Fresh);
    }

    #[test]
    fn freshness_fetch_failure() {
        let obs_recent = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 100,
            fetch_ok: false,
        };
        assert_eq!(freshness(&obs_recent, 110), Freshness::Unreachable);

        let obs_old = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 100,
            fetch_ok: false,
        };
        assert_eq!(freshness(&obs_old, 300), Freshness::Unreachable);

        let obs_future = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 300,
            fetch_ok: false,
        };
        assert_eq!(freshness(&obs_future, 100), Freshness::Unreachable);
    }

    #[test]
    fn display_state_for_each_freshness() {
        let fresh_obs = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 100,
            fetch_ok: true,
        };
        assert_eq!(display_state(&fresh_obs, 150), "running");

        let stale_obs = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 100,
            fetch_ok: true,
        };
        assert_eq!(display_state(&stale_obs, 200), "running (stale)");

        let unreachable_obs = CloudObservation {
            last_state: "running".to_string(),
            last_update_secs: 100,
            fetch_ok: false,
        };
        assert_eq!(
            display_state(&unreachable_obs, 150),
            "running (stale, unreachable)"
        );
    }

    #[test]
    fn display_state_preserves_terminal_states() {
        let succeeded_fresh = CloudObservation {
            last_state: "succeeded".to_string(),
            last_update_secs: 100,
            fetch_ok: true,
        };
        assert_eq!(display_state(&succeeded_fresh, 150), "succeeded");

        let succeeded_stale = CloudObservation {
            last_state: "succeeded".to_string(),
            last_update_secs: 100,
            fetch_ok: true,
        };
        assert_eq!(display_state(&succeeded_stale, 250), "succeeded (stale)");

        let failed_unreachable = CloudObservation {
            last_state: "failed".to_string(),
            last_update_secs: 100,
            fetch_ok: false,
        };
        assert_eq!(
            display_state(&failed_unreachable, 150),
            "failed (stale, unreachable)"
        );
    }

    #[test]
    fn terminal_list_exact() {
        // The exactly four lowercase terminal states
        assert!(is_terminal_state("succeeded"));
        assert!(is_terminal_state("failed"));
        assert!(is_terminal_state("timed_out"));
        assert!(is_terminal_state("cancelled"));

        // Capitalised variants are not terminal
        assert!(!is_terminal_state("Succeeded"));
        assert!(!is_terminal_state("Failed"));
        assert!(!is_terminal_state("Timed_out"));
        assert!(!is_terminal_state("Cancelled"));
        assert!(!is_terminal_state("SUCCEEDED"));

        // Non-terminal states
        assert!(!is_terminal_state("running"));
        assert!(!is_terminal_state("pending"));
        assert!(!is_terminal_state("queued"));
        assert!(!is_terminal_state(""));
        assert!(!is_terminal_state("unknown"));
        assert!(!is_terminal_state("succeeded (stale)"));
    }

    #[test]
    fn merge_ordering_with_ties_and_shared_id() {
        let local = vec![
            Row {
                id: "shared-id".to_string(),
                created_secs: 20,
                target: "local".to_string(),
                state: "running".to_string(),
            },
            Row {
                id: "b-task".to_string(),
                created_secs: 10,
                target: "local".to_string(),
                state: "succeeded".to_string(),
            },
            Row {
                id: "a-task".to_string(),
                created_secs: 10,
                target: "local".to_string(),
                state: "failed".to_string(),
            },
        ];

        let cloud = vec![
            Row {
                id: "shared-id".to_string(),
                created_secs: 20,
                target: "cloud".to_string(),
                state: "pending".to_string(),
            },
            Row {
                id: "c-task".to_string(),
                created_secs: 5,
                target: "cloud".to_string(),
                state: "queued".to_string(),
            },
        ];

        let merged = merge_rows(local, cloud);

        assert_eq!(merged.len(), 5);

        // created_secs = 5
        assert_eq!(merged[0].id, "c-task");
        assert_eq!(merged[0].created_secs, 5);
        assert_eq!(merged[0].target, "cloud");

        // created_secs = 10, tie-broken by id: "a-task" < "b-task"
        assert_eq!(merged[1].id, "a-task");
        assert_eq!(merged[1].created_secs, 10);
        assert_eq!(merged[2].id, "b-task");
        assert_eq!(merged[2].created_secs, 10);

        // created_secs = 20, shared id tie: stable order keeps local before cloud
        assert_eq!(merged[3].id, "shared-id");
        assert_eq!(merged[3].created_secs, 20);
        assert_eq!(merged[3].target, "local");

        assert_eq!(merged[4].id, "shared-id");
        assert_eq!(merged[4].created_secs, 20);
        assert_eq!(merged[4].target, "cloud");
    }

    #[test]
    fn merge_empty_inputs() {
        assert_eq!(merge_rows(vec![], vec![]), vec![]);

        let cloud_only = vec![
            Row {
                id: "task-2".to_string(),
                created_secs: 30,
                target: "cloud".to_string(),
                state: "running".to_string(),
            },
            Row {
                id: "task-1".to_string(),
                created_secs: 10,
                target: "cloud".to_string(),
                state: "succeeded".to_string(),
            },
        ];
        let merged_cloud = merge_rows(vec![], cloud_only);
        assert_eq!(merged_cloud.len(), 2);
        assert_eq!(merged_cloud[0].id, "task-1");
        assert_eq!(merged_cloud[1].id, "task-2");

        let local_only = vec![
            Row {
                id: "task-2".to_string(),
                created_secs: 30,
                target: "local".to_string(),
                state: "running".to_string(),
            },
            Row {
                id: "task-1".to_string(),
                created_secs: 10,
                target: "local".to_string(),
                state: "succeeded".to_string(),
            },
        ];
        let merged_local = merge_rows(local_only, vec![]);
        assert_eq!(merged_local.len(), 2);
        assert_eq!(merged_local[0].id, "task-1");
        assert_eq!(merged_local[1].id, "task-2");
    }
}
