//! The threshold-action registry: what happens to a start once a limit breaches.
//!
//! Actions run in registration order. The first one that returns [`ActionOutcome::Defer`] decides
//! the start. [`DeferStartAction`] is always last and always defers, so a breach never admits.

use std::fmt;

use serde::Serialize;

use super::{Breach, Evaluation, ResourceKind};
use crate::state::TaskId;

/// The kind of start being admitted. Serialized names are the `--kind` values of
/// `mcao resources admit`, which is not yet wired into the CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartKind {
    Ship,
    Scout,
    Secondmate,
    Relaunch,
}

/// One start the host may admit. `task` is the id the start would create or relaunch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartRequest {
    pub kind: StartKind,
    pub task: TaskId,
}

/// A refused start: the breaches that caused it and the one-line message the caller shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Deferral {
    pub breaches: Vec<Breach>,
    pub message: String,
}

/// What one action decided. Exhaustive on purpose: a new variant breaks every `match` until it is
/// handled.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionOutcome {
    /// Leave the decision to the next action.
    Continue,
    /// Refuse the start. Later actions do not run.
    Defer(Deferral),
}

/// An action that could not decide. The registry skips it, so it never admits a start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionError {
    pub reason: String,
}

impl fmt::Display for ActionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

/// One step of the threshold-action chain. It decides from the request and the evaluation only,
/// and must not start a process or change the host.
pub trait ThresholdAction {
    /// The name the registry's warning uses.
    fn name(&self) -> &'static str;

    /// Decides this request. Called only when at least one limit breaches.
    fn run(&self, req: &StartRequest, eval: &Evaluation) -> Result<ActionOutcome, ActionError>;
}

/// The ordered threshold actions that an admission runs when a limit breaches.
///
/// Implement [`ThresholdAction`] and pass it to [`ActionRegistry::register`] to add an action. It
/// runs in registration order, before [`DeferStartAction`]. Return [`ActionOutcome::Continue`] to
/// leave the decision to the next action, or [`ActionOutcome::Defer`] to refuse with your own
/// [`Deferral`]. An action that returns an error is skipped with a warning, so it never admits a
/// start on its own.
///
/// # Adding an action
///
/// 1. Implement [`ThresholdAction`] for a type that decides from the request and the evaluation
///    only. It must not start a process or change the host.
/// 2. Add one `register` call in [`default_registry`]. `register` places the action before
///    [`DeferStartAction`], so registration order is run order.
/// 3. A new [`ActionOutcome`] variant fails to compile in every `match` on the outcome until that
///    `match` handles it. In this slice the only such `match` is the loop behind
///    [`ActionRegistry::run`].
pub struct ActionRegistry {
    /// Always ends with [`DeferStartAction`], so the vector is never empty.
    actions: Vec<Box<dyn ThresholdAction>>,
}

impl Default for ActionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionRegistry {
    /// A registry holding only the terminal [`DeferStartAction`].
    pub fn new() -> Self {
        Self {
            actions: vec![Box::new(DeferStartAction)],
        }
    }

    /// Adds `action` ahead of [`DeferStartAction`], so it runs before any refusal.
    pub fn register(&mut self, action: Box<dyn ThresholdAction>) {
        let terminal = self.actions.len() - 1;
        self.actions.insert(terminal, action);
    }

    /// Runs the actions in order and returns the first refusal.
    ///
    /// The result is always a [`Deferral`], because [`DeferStartAction`] refuses. An action that
    /// returns an error is reported on standard error and skipped. The crate has no logging
    /// dependency, so the warning is written with `eprintln!`.
    pub fn run(&self, req: &StartRequest, eval: &Evaluation) -> Deferral {
        self.run_reporting(req, eval, &mut |line: &str| eprintln!("{line}"))
    }

    /// [`ActionRegistry::run`] with each skipped action's warning passed to `report`, so a test can
    /// read it instead of standard error.
    fn run_reporting(
        &self,
        req: &StartRequest,
        eval: &Evaluation,
        report: &mut dyn FnMut(&str),
    ) -> Deferral {
        for action in &self.actions {
            match action.run(req, eval) {
                Ok(ActionOutcome::Defer(deferral)) => return deferral,
                Ok(ActionOutcome::Continue) => {}
                Err(error) => report(&format!(
                    "warning: threshold action {} failed: {error}; continuing",
                    action.name()
                )),
            }
        }
        // Not reached while DeferStartAction is registered. Keeping this refusal here means run
        // needs no panic path.
        refuse(req, &eval.breaches)
    }
}

/// The registry that admission uses. To add an action for admission, bind [`ActionRegistry::new`]
/// to a `mut` variable, call [`ActionRegistry::register`] on it, and return it. See "Adding an
/// action" on [`ActionRegistry`].
pub fn default_registry() -> ActionRegistry {
    ActionRegistry::new()
}

/// The terminal action. It refuses the start whenever a limit breaches, so it is always last.
pub struct DeferStartAction;

impl ThresholdAction for DeferStartAction {
    fn name(&self) -> &'static str {
        "defer-start"
    }

    fn run(&self, req: &StartRequest, eval: &Evaluation) -> Result<ActionOutcome, ActionError> {
        Ok(ActionOutcome::Defer(refuse(req, &eval.breaches)))
    }
}

/// Builds the refusal for `breaches`. The message is the line the caller shows, so it names each
/// breach and the limit it crossed.
fn refuse(req: &StartRequest, breaches: &[Breach]) -> Deferral {
    let parts = breaches
        .iter()
        .map(|breach| {
            format!(
                "{} {} {} {}",
                breach.kind.key(),
                format_value(breach.kind, breach.value),
                comparison(breach.kind),
                format_value(breach.kind, breach.limit)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Deferral {
        breaches: breaches.to_vec(),
        message: format!(
            "deferred: host resources are past a threshold ({parts}); task {} was not launched and its backlog item stays queued - dispatch it again once usage is back within the thresholds (docs/configuration.md \"Host resource gate\")",
            req.task.as_str()
        ),
    }
}

/// Disk free space breaches below its limit, and every other resource breaches at or above its
/// limit. This matches `breaches_limit` in `evaluate.rs`.
fn comparison(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::DiskFreePct => "<",
        ResourceKind::MemoryUsedPct
        | ResourceKind::SwapUsedPct
        | ResourceKind::CpuUsedPct
        | ResourceKind::RunningAgents => ">=",
    }
}

/// Running agents print as whole numbers. Percentages use `Debug` formatting, so `90.0` keeps its
/// decimal point.
fn format_value(kind: ResourceKind, value: f64) -> String {
    match kind {
        ResourceKind::RunningAgents => format!("{value:.0}"),
        ResourceKind::MemoryUsedPct
        | ResourceKind::SwapUsedPct
        | ResourceKind::CpuUsedPct
        | ResourceKind::DiskFreePct => format!("{value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ResourceSnapshot, State};

    /// Fails every time it runs, so the registry must skip it.
    struct Broken;

    impl ThresholdAction for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }

        fn run(
            &self,
            _req: &StartRequest,
            _eval: &Evaluation,
        ) -> Result<ActionOutcome, ActionError> {
            Err(ActionError {
                reason: "probe broke".to_owned(),
            })
        }
    }

    fn memory_breached() -> Evaluation {
        Evaluation {
            snapshot: ResourceSnapshot {
                sampled_at: String::new(),
                platform: String::new(),
                readings: Vec::new(),
            },
            levels: Vec::new(),
            breaches: vec![Breach {
                kind: ResourceKind::MemoryUsedPct,
                value: 90.0,
                limit: 90.0,
            }],
            state: State::Breached,
            config_source: "defaults".to_owned(),
            config_warning: None,
        }
    }

    #[test]
    fn failing_action_warns_and_the_start_still_defers() {
        let mut registry = ActionRegistry::new();
        registry.register(Box::new(Broken));
        let req = StartRequest {
            kind: StartKind::Ship,
            task: TaskId::parse("ship-1").expect("fixture task id is valid"),
        };
        let eval = memory_breached();
        let mut warnings = Vec::new();

        let deferral = registry.run_reporting(&req, &eval, &mut |line: &str| {
            warnings.push(line.to_owned());
        });

        assert_eq!(
            warnings,
            ["warning: threshold action broken failed: probe broke; continuing"]
        );
        assert_eq!(deferral.breaches, eval.breaches);
    }
}
