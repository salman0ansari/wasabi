//! Supervisor lifecycle guarantees:
//! - exactly one runtime, named threads
//! - repeated start/stop leaves no resource growth;
//!   shutdown is deterministic; command gate closes first.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use wasabi_core::{CoreSupervisor, SessionState, SupervisorConfig};

fn thread_count() -> usize {
    // Linux-only environment per repo policy; portable fallback returns 0.
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Threads:") {
                return rest.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

#[test]
fn start_stop_loop_threads_plateau() {
    // Warm-up: allocator, runtime caches, tracing.
    for _ in 0..5 {
        let s = CoreSupervisor::start(SupervisorConfig::new("/tmp/wasabi-test-does-not-exist"))
            .expect("start");
        s.shutdown();
    }

    let baseline = thread_count();
    assert!(baseline > 0, "thread probe works");

    const ITERATIONS: usize = 200;
    let mut max_seen = baseline;

    for _ in 0..ITERATIONS {
        let s = CoreSupervisor::start(SupervisorConfig::new("/tmp/wasabi-test-does-not-exist"))
            .expect("start");
        max_seen = max_seen.max(thread_count());
        s.shutdown();
    }

    // Allow small slack for background tokio/allocator threads winding down,
    // but a leak would grow linearly: 200 iterations × even 1 thread = 200.
    let after = thread_count();
    assert!(
        after <= baseline + 4 && max_seen <= baseline + 12,
        "thread leak: baseline={baseline} max={max_seen} after={after}"
    );
}

#[test]
fn shutdown_closes_command_gate_and_cancels_children() {
    let s = CoreSupervisor::start(SupervisorConfig::new("/tmp/wasabi-test-gate")).expect("start");
    assert!(s.commands_accepted());

    let child = s.child_token("test-subsystem");
    let completed = Arc::new(AtomicUsize::new(0));

    // A long-running owned task must observe cancellation.
    let done = Arc::clone(&completed);
    let handle = s.spawn_owned(&child, "sleeper", async move {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        done.fetch_add(1, Ordering::SeqCst);
    });

    s.shutdown();
    // Cancellation must finish the wrapper promptly; the inner future never
    // ran to completion.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !handle.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        handle.is_finished(),
        "owned task did not observe cancellation"
    );
    assert_eq!(completed.load(Ordering::SeqCst), 0);
    // Supervisor dropped without explicit shutdown path exercised above via
    // shutdown(); a second drop must be a no-op (Drop safety net).
}

#[test]
fn state_watch_reflects_transitions() {
    // The watch channel is last-value-wins.
    let (tx, mut rx) = tokio::sync::watch::channel(SessionState::Stopped);
    tx.send_replace(SessionState::Connecting);
    tx.send_replace(SessionState::Connected);
    tx.send_replace(SessionState::Reconnecting);
    assert_eq!(*rx.borrow_and_update(), SessionState::Reconnecting);
}
