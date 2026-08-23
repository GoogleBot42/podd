//! Subsystem health registry — a latest-value `watch` of "which parts of podd
//! are actually working right now".
//!
//! Mirrors the [`crate::bus`] state fan-out: the managers are the sole
//! producers (they call [`HealthRegistry::report`] at transitions they already
//! detect and log), `api` is a read-only subscriber that renders the map as
//! `GET /api/serverStatus`.
//!
//! **Observation only.** Nothing here may influence actuation, retry timing or
//! serial behaviour: `report` is a pure publish, it never blocks, never fails,
//! and its result is never inspected by a caller. A manager that stops
//! reporting simply leaves the last transition standing (with its timestamp),
//! which is exactly what a status page wants to show.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::watch;

/// Stable subsystem keys. These are the wire keys of `GET /api/serverStatus`
/// (`api`'s `ServerStatus` struct mirrors them), so don't rename them casually.
pub const SENSOR: &str = "sensor";
pub const COVER_CONTROL: &str = "coverControl";
pub const MQTT: &str = "mqtt";
pub const CLOCK: &str = "clock";

/// Every subsystem the registry tracks, seeded [`Health::NotStarted`].
/// (`api` reports itself in the handler — if it answered, it is up.)
pub const SUBSYSTEMS: &[&str] = &[SENSOR, COVER_CONTROL, MQTT, CLOCK];

/// Health of one subsystem. Deliberately the same six states as the HTTP wire's
/// `Status` enum (and the UI's `statusMeta`) so no mapping invents a value the
/// UI can't render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Health {
    /// Nothing has reported yet (process just came up).
    #[default]
    NotStarted,
    /// Running, but not yet confirmed fully working (e.g. the sensor MCU's
    /// ~60 s post-restart window where it streams telemetry but eats writes).
    Started,
    /// Coming back after a failure.
    Restarting,
    /// Failing and being retried automatically.
    Retrying,
    /// Given up / not working, no automatic recovery in progress.
    Failed,
    /// Working normally.
    Healthy,
}

/// One subsystem's latest reported state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subsystem {
    pub health: Health,
    /// Human-readable detail for the status page ("connected", the error text
    /// of the current retry loop, ...).
    pub message: String,
    /// When this state was *entered* (not when it was last re-reported).
    pub since: jiff::Timestamp,
}

/// Subsystem key -> latest state. `BTreeMap` so iteration order is stable.
pub type HealthMap = BTreeMap<String, Subsystem>;

/// Producer handle, cloned into each manager.
#[derive(Clone)]
pub struct HealthRegistry {
    tx: Arc<watch::Sender<HealthMap>>,
}

impl HealthRegistry {
    /// Create a registry with every [`SUBSYSTEMS`] entry seeded
    /// [`Health::NotStarted`], plus the consumer-side receiver.
    pub fn new() -> (Self, watch::Receiver<HealthMap>) {
        let now = jiff::Timestamp::now();
        let seed: HealthMap = SUBSYSTEMS
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    Subsystem {
                        health: Health::NotStarted,
                        message: "not started yet".to_string(),
                        since: now,
                    },
                )
            })
            .collect();
        let (tx, rx) = watch::channel(seed);
        (
            HealthRegistry {
                tx: Arc::new(tx),
            },
            rx,
        )
    }

    /// Record `name`'s current state.
    ///
    /// A repeat of the state already recorded (same health *and* message) is
    /// dropped, so `since` keeps meaning "when this state was entered" and
    /// hot-loop callers don't wake every subscriber.
    pub fn report(&self, name: &str, health: Health, message: impl Into<String>) {
        let message = message.into();
        self.tx.send_if_modified(|map| {
            if let Some(existing) = map.get(name)
                && existing.health == health
                && existing.message == message
            {
                return false;
            }
            map.insert(
                name.to_string(),
                Subsystem {
                    health,
                    message,
                    since: jiff::Timestamp::now(),
                },
            );
            true
        });
    }

    /// Current snapshot (tests / diagnostics).
    pub fn snapshot(&self) -> HealthMap {
        self.tx.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_every_subsystem_not_started() {
        let (reg, rx) = HealthRegistry::new();
        let map = rx.borrow().clone();
        assert_eq!(map.len(), SUBSYSTEMS.len());
        for name in SUBSYSTEMS {
            assert_eq!(map[*name].health, Health::NotStarted);
        }
        drop(reg);
    }

    #[test]
    fn report_updates_state_and_notifies() {
        let (reg, mut rx) = HealthRegistry::new();
        rx.borrow_and_update();
        reg.report(SENSOR, Health::Healthy, "connected");
        assert!(rx.has_changed().unwrap());
        let map = rx.borrow_and_update().clone();
        assert_eq!(map[SENSOR].health, Health::Healthy);
        assert_eq!(map[SENSOR].message, "connected");
    }

    #[test]
    fn repeat_report_is_dropped() {
        let (reg, mut rx) = HealthRegistry::new();
        reg.report(SENSOR, Health::Healthy, "connected");
        let first = rx.borrow_and_update()[SENSOR].since;
        reg.report(SENSOR, Health::Healthy, "connected");
        assert!(!rx.has_changed().unwrap());
        assert_eq!(rx.borrow()[SENSOR].since, first);
    }

    #[test]
    fn transition_moves_the_timestamp() {
        let (reg, rx) = HealthRegistry::new();
        reg.report(SENSOR, Health::Healthy, "connected");
        let first = rx.borrow()[SENSOR].since;
        reg.report(SENSOR, Health::Retrying, "sensor not responding");
        let map = rx.borrow().clone();
        assert_eq!(map[SENSOR].health, Health::Retrying);
        assert!(map[SENSOR].since >= first);
    }

    #[test]
    fn unknown_names_are_accepted() {
        let (reg, rx) = HealthRegistry::new();
        reg.report("something-else", Health::Failed, "boom");
        assert_eq!(rx.borrow()["something-else"].health, Health::Failed);
    }
}
