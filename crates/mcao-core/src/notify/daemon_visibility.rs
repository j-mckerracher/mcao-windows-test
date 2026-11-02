//! The [`DaemonVisibility`] trait: the wedge alarm that surfaces stalled away-mode escalations.

use crate::McaoError;

/// Posts a wedge alarm carrying `summary` through the platform's notification channel.
pub trait DaemonVisibility {
    /// Posts the alarm. A channel that is absent or refuses the alarm is reported as an error.
    fn wedge_alarm(&self, summary: &str) -> Result<(), McaoError>;
}
