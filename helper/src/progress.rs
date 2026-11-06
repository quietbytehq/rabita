// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 QuietByte

//! Live progress for long jobs (join, leave).
//!
//! Jobs report through a [`ProgressSink`]. The D-Bus service turns events
//! into signals; the CLI prints them; tests collect them.

use std::sync::{Mutex, MutexGuard};

use crate::types::{StepOutcome, StepState};

/// Something that happened during a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// A step changed state.
    Step {
        /// Step id.
        id: String,
        /// Plain-language label.
        label: String,
        /// New state.
        state: StepState,
        /// Plain-language message.
        message: String,
    },
    /// A line for the technical log ("Show details").
    Log(String),
}

/// Receives progress events.
pub trait ProgressSink: Send + Sync {
    /// Handles one event.
    fn event(&self, event: ProgressEvent);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Collects events in memory (tests, and the final result).
#[derive(Debug, Default)]
pub struct Collector {
    events: Mutex<Vec<ProgressEvent>>,
}

impl Collector {
    /// Creates an empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything received so far.
    #[must_use]
    pub fn events(&self) -> Vec<ProgressEvent> {
        lock(&self.events).clone()
    }

    /// The last state of every step, in first-seen order.
    #[must_use]
    pub fn outcomes(&self) -> Vec<StepOutcome> {
        let mut out: Vec<StepOutcome> = Vec::new();
        for ev in self.events() {
            if let ProgressEvent::Step {
                id,
                label,
                state,
                message,
            } = ev
            {
                if let Some(existing) = out.iter_mut().find(|o| o.id == id) {
                    existing.state = state;
                    existing.message = message;
                } else {
                    out.push(StepOutcome {
                        id,
                        label,
                        state,
                        message,
                    });
                }
            }
        }
        out
    }
}

impl ProgressSink for Collector {
    fn event(&self, event: ProgressEvent) {
        lock(&self.events).push(event);
    }
}

/// Forwards to a closure (used by the D-Bus service and the CLI).
pub struct FnSink<F: Fn(ProgressEvent) + Send + Sync>(pub F);

impl<F: Fn(ProgressEvent) + Send + Sync> ProgressSink for FnSink<F> {
    fn event(&self, event: ProgressEvent) {
        (self.0)(event);
    }
}

/// Sends every event to several sinks.
pub struct Tee<'a>(pub Vec<&'a dyn ProgressSink>);

impl ProgressSink for Tee<'_> {
    fn event(&self, event: ProgressEvent) {
        for s in &self.0 {
            s.event(event.clone());
        }
    }
}

/// Convenience wrapper that a job uses to report on one step at a time.
pub struct Reporter<'a> {
    sink: &'a dyn ProgressSink,
}

impl<'a> Reporter<'a> {
    /// Wraps a sink.
    #[must_use]
    pub fn new(sink: &'a dyn ProgressSink) -> Self {
        Self { sink }
    }

    /// Emits a step event.
    pub fn step(&self, id: &str, label: &str, state: StepState, message: &str) {
        self.sink.event(ProgressEvent::Step {
            id: id.to_owned(),
            label: label.to_owned(),
            state,
            message: message.to_owned(),
        });
    }

    /// Emits a log line.
    pub fn log(&self, line: impl Into<String>) {
        self.sink.event(ProgressEvent::Log(line.into()));
    }

    /// Logs a command and its output (standard input is never logged).
    pub fn log_command(&self, display: &str, output: &crate::cmd::CommandOutput) {
        self.log(format!("$ {display}"));
        for line in output.combined().lines() {
            self.log(format!("  {line}"));
        }
        match output.code {
            Some(0) => {}
            Some(c) => self.log(format!("  (exit status {c})")),
            None if output.timed_out => self.log("  (timed out)"),
            None => self.log("  (killed)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_tracks_last_state_per_step() {
        let c = Collector::new();
        let r = Reporter::new(&c);
        r.step("a", "A", StepState::Running, "");
        r.log("hello");
        r.step("b", "B", StepState::Running, "");
        r.step("a", "A", StepState::Done, "fine");
        let out = c.outcomes();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[0].state, StepState::Done);
        assert_eq!(out[0].message, "fine");
        assert_eq!(out[1].state, StepState::Running);
        assert_eq!(c.events().len(), 4);
    }
}
