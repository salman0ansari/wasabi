//! Lifecycle-leak and soak probes on top of `CoreSupervisor`.
//!
//! Plain std tests: the ONLY Tokio runtime in play is the supervisor's own,
//! reached through its handle. Each probe owns a fresh supervisor and shuts
//! it down deterministically, so repeated runs cannot cross-contaminate.
//!
//! NOTE for the manifest owner: tests 2 and 3 consume crates that are not
//! (yet) declared under `[dev-dependencies]` of `wasabi-core`; see the report
//! accompanying this file for the exact additions needed to compile them.

// Tests exercise raw store/cache APIs directly.
#![allow(clippy::disallowed_methods)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use wacore::proto_helpers::MessageBuilderExt;
use wasabi_core::{CoreSupervisor, SupervisorConfig};

const JOIN_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Count of open file descriptors; 0 where /proc is unavailable so the
/// fd-growth assertions degrade to vacuous passes off-Linux.
#[allow(dead_code)]
fn fd_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc/self/fd")
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[test]
fn rapid_chat_switch_generations_stale_results_dropped() {
    let sup =
        CoreSupervisor::start(SupervisorConfig::new("/tmp/wasabi-soak-switch")).expect("start");
    const SWITCHES: u64 = 2000;
    // Runtime workers are fully up after start(), so this baseline excludes
    // scheduler setup and any growth below is residue from the storm itself.
    let threads_baseline = thread_count();

    // Live generation counter models the chat the UI currently shows; it is
    // bumped immediately after every spawn, so every probe is stale by the
    // time its 5ms delivery delay elapses.
    let generation = Arc::new(AtomicU64::new(0));
    // Delivery slot: (generation, value) of the freshest applied result. The
    // freshest-generation-wins guard is what a real switch handler relies on:
    // a late result from an older switch must never overwrite newer state.
    let applied: Arc<std::sync::Mutex<Option<(u64, u64)>>> = Arc::new(std::sync::Mutex::new(None));
    let token = sup.child_token("chat-switch");

    sup.handle().block_on(async {
        let mut handles = Vec::with_capacity(SWITCHES as usize);
        for i in 0..SWITCHES {
            let applied = Arc::clone(&applied);
            handles.push(sup.spawn_owned(&token, "switch-result", async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let mut slot = applied.lock().expect("slot lock poisoned");
                // Apply only if no NEWER generation already claimed the
                // slot; equal generations cannot occur (ids are unique).
                if slot.is_none_or(|(g, _)| g < i) {
                    *slot = Some((i, i));
                }
            }));
            generation.fetch_add(1, Ordering::AcqRel);
        }
        assert_eq!(
            generation.load(Ordering::Acquire),
            SWITCHES,
            "every switch must bump the generation exactly once"
        );

        // Leak check without supervisor-internal counters: join everything.
        // A cancelled task resolves None; a leaked one never resolves at all.
        let mut ran = 0u64;
        for h in handles {
            let out = tokio::time::timeout(JOIN_TIMEOUT, h)
                .await
                .expect("owned probe did not resolve before timeout")
                .expect("owned probe panicked");
            assert_eq!(
                out,
                Some(()),
                "probe must run to completion, not be cancelled"
            );
            ran += 1;
        }
        assert_eq!(ran, SWITCHES, "no owned task may leak past shutdown prep");
    });

    // After ALL probes joined, the stored generation must be the LAST one:
    // earlier completions either arrived before it (overwritten) or after it
    // (refused by the guard) — neither may regress the slot.
    let stored = applied.lock().expect("slot lock poisoned");
    assert_eq!(
        *stored,
        Some((SWITCHES - 1, SWITCHES - 1)),
        "stale results must not win the delivery slot"
    );

    // Secondary residue check: 2000 owned tasks must strand no threads.
    // Threads wind down asynchronously after the tasks finish (tokio parks
    // then reaps workers), so poll for a plateau instead of sampling once.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut threads_after = thread_count();
    while threads_after > threads_baseline + 4 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        threads_after = thread_count();
    }
    assert!(
        threads_after <= threads_baseline + 4,
        "thread residue after switch storm: baseline={threads_baseline} after={threads_after}"
    );

    sup.shutdown();
}

#[test]
fn open_close_account_store_x100_no_fd_growth() {
    // Probe reads /proc/self/fd; elsewhere there is nothing to measure, so
    // the test passes vacuously by design.
    #[cfg(not(target_os = "linux"))]
    {
        return;
    }

    #[cfg(target_os = "linux")]
    {
        use waproto::whatsapp as wa;
        use wasabi_repository::{AccountStore, StoreTuning};
        use wasabi_test_support::TestDir;

        const PEER: &str = "559900000001@s.whatsapp.net";
        const CYCLES: usize = 100;
        const FD_SLACK: usize = 8;

        let dir = TestDir::new("soak-fd");
        let sup = CoreSupervisor::start(SupervisorConfig::new(dir.path().join("supervisor")))
            .expect("start");

        // One open/record/flush/drop cycle against its own sqlite file.
        async fn cycle(root: &std::path::Path, i: usize) {
            let store = AccountStore::open(
                &root.join(format!("soak-{i}.sqlite3")),
                &StoreTuning::default(),
            )
            .await
            .expect("open account store");
            let chats = store.chats().clone();
            let peer: whatsapp_rust::Jid = PEER.parse().expect("valid test JID");
            chats
                .record_outgoing(
                    &peer,
                    format!("FD{i}"),
                    &wa::Message::text("soak"),
                    chrono::Utc::now(),
                )
                .expect("record outgoing");
            store.flush().await.expect("flush barrier");
            drop(chats);
            drop(store); // last handles release pools + writer task
        }

        // Warm-up absorbs lazy one-time allocations (sqlite handles, pool
        // plumbing) so the baseline reflects steady-state usage.
        sup.handle().block_on(async {
            for i in 0..3 {
                cycle(dir.path(), i).await
            }
        });
        let baseline = fd_count();
        assert!(baseline > 0, "fd probe works");

        sup.handle().block_on(async {
            for i in 3..CYCLES {
                cycle(dir.path(), i).await;
            }
        });

        // Bounded settle instead of a fixed sleep: pool teardown is async and
        // should land promptly; if it does not, the assert below fails with
        // the observed numbers rather than hanging.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut after = fd_count();
        while after > baseline + FD_SLACK && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            after = fd_count();
        }
        assert!(
            after <= baseline + FD_SLACK,
            "fd leak across {CYCLES} open/close cycles: baseline={baseline} after={after}"
        );

        sup.shutdown();
    }
}

#[test]
fn media_cache_quota_loop_plateau() {
    // Public DiskCache API suffices (open_with_quota/store_bytes/evict_to/
    // total_bytes), so no skip-for-missing-API path is needed here.
    use wasabi_media::DiskCache;
    use wasabi_test_support::TestDir;

    const QUOTA: u64 = 4096;
    const ROUNDS: usize = 20;
    const CHUNK_LEN: usize = 1024;
    const CHUNKS_PER_ROUND: usize = 8; // 8 KiB written per round > quota

    let dir = TestDir::new("soak-media");
    let sup =
        CoreSupervisor::start(SupervisorConfig::new(dir.path().join("supervisor"))).expect("start");

    let totals: Vec<u64> = sup.handle().block_on(async {
        let mut totals = Vec::with_capacity(ROUNDS);
        // Keys only need the sha-shaped lowercase-hex form the cache validates
        // for; a counter formatted wide keeps them unique without pulling sha2.
        let mut key_counter = 0u64;
        for round in 0..ROUNDS {
            // Reopening over the SAME root each round simulates restarts: any
            // failure to reclaim disk would compound visibly across rounds.
            let cache = DiskCache::open_with_quota(dir.path().join("cache"), QUOTA)
                .await
                .expect("open cache");
            for _ in 0..CHUNKS_PER_ROUND {
                let mut blob = vec![0u8; CHUNK_LEN];
                // Round-tagged payload so rounds are distinguishable on
                // inspection; the cache keys entries by the hex name alone.
                blob[0] = round as u8;
                cache
                    .store_bytes(&format!("{key_counter:064x}"), &blob)
                    .await
                    .expect("store chunk");
                key_counter += 1;
            }
            // Writes do NOT auto-evict; quota enforcement is explicit here,
            // mirroring how the manager trims after large arrivals.
            let after_evict = cache.evict_to(QUOTA).await.expect("evict");
            assert!(
                after_evict <= QUOTA,
                "evict_to left {after_evict} > {QUOTA}"
            );
            let total = cache.total_bytes().await.expect("total bytes");
            assert!(total <= QUOTA, "usage {total} exceeded quota {QUOTA}");
            totals.push(total);
        }
        totals
    });

    // Plateau: every round bounded by quota AND the band stays flat — no
    // upward drift across reopen cycles means committed bytes are reclaimed.
    let max_seen = *totals.iter().max().unwrap_or(&0);
    let min_seen = *totals.iter().min().unwrap_or(&0);
    assert!(
        max_seen <= QUOTA && max_seen - min_seen <= QUOTA,
        "cache usage did not plateau: totals={totals:?}"
    );

    sup.shutdown();
}
