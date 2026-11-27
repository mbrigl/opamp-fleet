//! The timers a Plugin sets, which arrive as inputs (CLT-wrappers-act-through-the-core §1 and
//! §4). A Plugin reads no clock: time reaches it as the input of a timer it set.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::AbortHandle;

/// A timer that fired: its id and the setting it fired for.
pub(super) type Fired = (u64, u64);

/// The armed timers of one instance, each by id with the setting that armed it.
pub(super) struct Timers {
    armed: Mutex<HashMap<u64, (u64, AbortHandle)>>,
    settings: AtomicU64,
    fired: mpsc::UnboundedSender<Fired>,
}

impl Timers {
    /// No timer armed yet, and the receiver that carries each one that fires.
    pub(super) fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<Fired>) {
        let (fired, receiver) = mpsc::unbounded_channel();
        (
            Arc::new(Timers {
                armed: Mutex::new(HashMap::new()),
                settings: AtomicU64::new(0),
                fired,
            }),
            receiver,
        )
    }

    /// Arms `id` to fire once after `after`, replacing what it was armed for before.
    pub(super) fn set(&self, id: u64, after: Duration) {
        let setting = self.settings.fetch_add(1, Ordering::Relaxed);
        let fired = self.fired.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let _ = fired.send((id, setting));
        });
        if let Ok(mut armed) = self.armed.lock() {
            if let Some((_, replaced)) = armed.insert(id, (setting, task.abort_handle())) {
                replaced.abort();
            }
        }
    }

    /// Disarms `id`. A firing already on its way is dropped by [`take`](Self::take).
    pub(super) fn cancel(&self, id: u64) {
        if let Ok(mut armed) = self.armed.lock() {
            if let Some((_, task)) = armed.remove(&id) {
                task.abort();
            }
        }
    }

    /// Whether `fired` is still the timer's current setting, disarming it if so: a timer
    /// cancelled or set again after it fired never arrives.
    pub(super) fn take(&self, (id, setting): Fired) -> bool {
        let Ok(mut armed) = self.armed.lock() else {
            return false;
        };
        if armed
            .get(&id)
            .is_some_and(|(current, _)| *current == setting)
        {
            armed.remove(&id);
            return true;
        }
        false
    }
}

impl Drop for Timers {
    /// The timers of an instance that is gone fire for nobody: each armed one is disarmed.
    fn drop(&mut self) {
        if let Ok(armed) = self.armed.get_mut() {
            for (_, task) in armed.values() {
                task.abort();
            }
        }
    }
}
