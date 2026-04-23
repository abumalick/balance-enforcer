//! Pure reconciliation logic. No OS-specific code, fully testable with a mock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
use tracing::{debug, error, info, warn};

use crate::audio::{AudioController, AudioEvent, AudioResult, BalancePolicy};

/// Coalesce a burst of events within this window into a single reconcile pass.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(50);

pub struct BalanceEnforcer<C: AudioController> {
    controller: C,
    policy: BalancePolicy,
    stop: Arc<AtomicBool>,
}

impl<C: AudioController> BalanceEnforcer<C> {
    pub fn new(controller: C, policy: BalancePolicy) -> Self {
        Self {
            controller,
            policy,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Returns a flag that, when set to true, causes `run` to exit at the next event.
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Consume the enforcer and return the inner controller. Useful for tests
    /// that want to inspect the mock's recorded writes after the loop exits.
    pub fn into_controller(self) -> C {
        self.controller
    }

    /// Block the current thread driving reconciliation until `stop_flag` is set
    /// or the event channel is closed.
    pub fn run(&self) -> AudioResult<()> {
        let rx = self.controller.subscribe()?;
        info!(
            left = self.policy.left,
            right = self.policy.right,
            tolerance = self.policy.tolerance,
            "balance enforcer starting"
        );

        // One upfront reconciliation — in case the state is already drifted at startup.
        if let Err(e) = self.reconcile_once() {
            warn!(error = %e, "initial reconciliation failed; continuing");
        }

        while !self.stop.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(first_event) => {
                    debug!(?first_event, "received event");
                    self.drain_burst(&rx);
                    if let Err(e) = self.reconcile_once() {
                        error!(error = %e, "reconcile failed; continuing");
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Defensive: if the channel has gone quiet for a while, poke
                    // the device once. Cheap and catches edge cases where the
                    // Windows callback stopped firing silently.
                    if let Err(e) = self.reconcile_once() {
                        warn!(error = %e, "watchdog reconcile failed");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    warn!("event channel disconnected; stopping enforcer");
                    break;
                }
            }
        }

        info!("balance enforcer stopped");
        Ok(())
    }

    /// Runs at most one reconcile pass, returning errors instead of looping.
    /// Exposed for test access; not called externally from `main`.
    pub fn reconcile_once(&self) -> AudioResult<()> {
        let channels = self.controller.channel_count()?;
        if channels != 2 {
            warn!(
                channels,
                "skipping reconcile: only stereo (2 channels) is handled"
            );
            return Ok(());
        }

        self.reconcile_channel(0, self.policy.left)?;
        self.reconcile_channel(1, self.policy.right)?;
        Ok(())
    }

    fn reconcile_channel(&self, channel: u32, target: f32) -> AudioResult<()> {
        let current = self.controller.get_channel(channel)?;
        if (current - target).abs() < self.policy.tolerance {
            return Ok(());
        }
        debug!(channel, current, target, "writing channel level");
        self.controller.set_channel(channel, target)
    }

    fn drain_burst(&self, rx: &Receiver<AudioEvent>) {
        let deadline = Instant::now() + DEBOUNCE_WINDOW;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(remaining) {
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    }
}
