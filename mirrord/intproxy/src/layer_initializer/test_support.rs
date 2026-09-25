//! Deterministic pause at the boundary between decoding a layer PID and publishing it.
//! Tests release the pause only after shutdown begins, proving the registration is still drained.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::LayerInitializerShutdown;

#[derive(Debug)]
pub(crate) struct RegistrationGate {
    paused: AtomicBool,
    reached: Semaphore,
    release: Semaphore,
}

impl RegistrationGate {
    pub(crate) fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            reached: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }

    pub(crate) async fn pause_after_decode(&self) {
        if self.paused.load(Ordering::Relaxed) {
            self.reached.add_permits(1);
            self.release
                .acquire()
                .await
                .expect("registration gate unexpectedly closed")
                .forget();
        }
    }
}

#[derive(Clone)]
pub(crate) struct RegistrationGateControl {
    gate: Arc<RegistrationGate>,
    cancellation: CancellationToken,
}

impl RegistrationGateControl {
    pub(crate) fn new(shutdown: &LayerInitializerShutdown) -> Self {
        Self {
            gate: shutdown.registration_gate.clone(),
            cancellation: shutdown.cancellation.clone(),
        }
    }

    pub(crate) fn pause(&self) {
        self.gate.paused.store(true, Ordering::Relaxed);
    }

    pub(crate) async fn wait_until_reached(&self) {
        self.gate
            .reached
            .acquire()
            .await
            .expect("registration gate unexpectedly closed")
            .forget();
    }

    pub(crate) async fn wait_for_shutdown_request(&self) {
        self.cancellation.cancelled().await;
    }

    pub(crate) fn release(&self) {
        self.gate.release.add_permits(1);
    }
}
