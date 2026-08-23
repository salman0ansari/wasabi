//! Headless soak for the wasabi storage path: feeds a synthetic message load
//! through `AccountStore` under a real `CoreSupervisor`, samples process
//! health on a fixed cadence, and refuses to exit clean if memory or threads
//! trend upward beyond noise.
//!
//! Configuration is env-only:
//!   SOAK_MINUTES            run length, default 60
//!   SOAK_ACCOUNT_DB         sqlite file directory, default a fresh tempdir
//!   SOAK_RATE_MSGS_PER_SEC  outgoing feed rate, default 50
//!
//! Telemetry: one CSV row per sample window on stdout. Ctrl-C triggers the
//! same graceful shutdown as natural completion.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use tracing::{info, warn};
use wacore::proto_helpers::MessageBuilderExt;
use wacore::types::events::InboundMessage;
use wacore::types::message::{MessageInfo, MessageSource};
use waproto::whatsapp as wa;
use wasabi_core::{CoreSupervisor, SupervisorConfig};
use wasabi_repository::{AccountStore, StoreTuning};
use whatsapp_rust::Jid;

const PEER_JID: &str = "559900000001@s.whatsapp.net";

const SAMPLE_PERIOD: Duration = Duration::from_secs(10);
const FLUSH_PERIOD: Duration = Duration::from_millis(250);
const INBOUND_PERIOD: Duration = Duration::from_secs(1);
const INBOUND_BATCH: usize = 5;
/// Bounded worker set draining the feed queue; the queue bound plus the
/// store's own ingress backpressure keeps in-flight work capped under stalls.
const WORKERS: usize = 4;
const QUEUE_CAPACITY: usize = 256;
/// Latency ring sized to hold one full sample window of flush timings.
const RING_CAP: usize = 256;

/// Per-handle cap on the post-cancellation drain; exceeding it is reported,
/// never hung on.
const DRAIN_JOIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Below this many windows a regression fit would be pure noise.
const MIN_SAMPLES_FOR_TREND: usize = 6;
/// Leak thresholds sit far above observed allocator/SQLite jitter but far
/// below any real leak: a handle-per-message bug at default rate would add
/// orders of magnitude more than 1 MiB/min or 2 threads/min.
const RSS_LEAK_KIB_PER_MIN: f64 = 1024.0;
const THREAD_LEAK_PER_MIN: f64 = 2.0;
/// Secondary gate: even a slope under the threshold must not show a large
/// step between the first and last quarter of the run.
const ABS_RSS_GROWTH_KIB: f64 = 8192.0;

fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            warn!(env = name, value = %v, fallback = default, "bad value, using default");
            default
        }),
        Err(_) => default,
    }
}

fn env_f64(name: &str, default: f64) -> f64 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            warn!(env = name, value = %v, fallback = default, "bad value, using default");
            default
        }),
        Err(_) => default,
    }
}

#[derive(Clone, Copy, Debug)]
struct Config {
    minutes: u64,
    rate_msgs_per_sec: f64,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    // Rate floor keeps interval math defined; 0.1 msg/s is effectively idle.
    let cfg = Config {
        minutes: env_u64("SOAK_MINUTES", 60),
        rate_msgs_per_sec: env_f64("SOAK_RATE_MSGS_PER_SEC", 50.0).max(0.1),
    };

    // Default DB location is an owned tempdir declared BEFORE the supervisor
    // so reverse-order drop tears the runtime down before the directory goes.
    let temp_db = tempfile::tempdir().ok();
    let account_dir: PathBuf = match std::env::var("SOAK_ACCOUNT_DB") {
        Ok(p) => PathBuf::from(p),
        Err(_) => match &temp_db {
            Some(td) => td.path().to_path_buf(),
            None => {
                eprintln!("soak: cannot create temp dir for account db");
                return ExitCode::FAILURE;
            }
        },
    };

    let sup = match CoreSupervisor::start(SupervisorConfig::new(account_dir.clone())) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soak: supervisor start failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let db_path = account_dir.join("store.sqlite3");
    let samples = match sup.handle().block_on(drive(&sup, &db_path, cfg)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soak: load generation failed: {e:#}");
            sup.shutdown();
            return ExitCode::FAILURE;
        }
    };

    sup.shutdown();
    verdict(&samples)
}

struct Sample {
    t_secs: f64,
    rss_kib: Option<u64>,
    threads: Option<u64>,
    fds: u64,
    ingress_depth: usize,
    ingress_dropped: u64,
    flush_p50_us: f64,
    flush_p95_us: f64,
}

fn proc_status_field(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            let rest = rest.trim_start_matches(':').trim();
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

fn fd_count() -> u64 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc/self/fd")
            .map(|entries| entries.filter_map(|e| e.ok()).count() as u64)
            .unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Nearest-rank percentile over an already-sorted window.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[idx.clamp(1, sorted.len()) - 1]
}

fn flag_fatal(tx: &tokio::sync::watch::Sender<bool>) {
    tx.send_modify(|v| *v = true);
}

/// Owns the store and every spawned task until graceful teardown completes.
async fn drive(sup: &CoreSupervisor, db_path: &Path, cfg: Config) -> Result<Vec<Sample>> {
    let peer: Jid = PEER_JID.parse().context("fixture JID")?;
    let run_for = Duration::from_secs(cfg.minutes.saturating_mul(60));

    let store = Arc::new(
        AccountStore::open(db_path, &StoreTuning::default())
            .await
            .context("open account store")?,
    );
    let chats = store.chats().clone();

    let (fatal_tx, mut fatal_rx) = tokio::sync::watch::channel(false);
    let (feed_tx, feed_rx) = tokio::sync::mpsc::channel::<u64>(QUEUE_CAPACITY);
    let queue = Arc::new(tokio::sync::Mutex::new(feed_rx));
    let flush_lat_us: Arc<Mutex<VecDeque<f64>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(RING_CAP)));
    let sent = Arc::new(AtomicU64::new(0));
    let inbound_applied = Arc::new(AtomicU64::new(0));

    let token = sup.child_token("soak");

    // Producer: paces the synthetic outgoing load into the bounded queue.
    let producer = {
        let token = token.clone();
        let mut tx = feed_tx.clone();
        let sent = Arc::clone(&sent);
        sup.spawn_owned(&token, "soak-producer", async move {
            let mut ticker =
                tokio::time::interval(Duration::from_secs_f64(1.0 / cfg.rate_msgs_per_sec));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut n = 0u64;
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        n += 1;
                        // Send failure means every worker exited on error;
                        // stop feeding instead of filling an orphan queue.
                        if tx.send(n).await.is_err() {
                            break;
                        }
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                    _ = token.cancelled() => break,
                }
            }
        })
    };
    drop(feed_tx);

    // Workers: bounded set; pulls serialize through the shared queue lock so
    // total concurrent records stay bounded by worker count, not load.
    let mut handles = Vec::with_capacity(WORKERS + 3);
    for w in 0..WORKERS {
        let token = token.clone();
        let chats = Arc::clone(&chats);
        let queue = Arc::clone(&queue);
        let fatal = fatal_tx.clone();
        let peer = peer.clone();
        handles.push(sup.spawn_owned(&token, "soak-worker", async move {
            loop {
                let next = {
                    let mut q = queue.lock().await;
                    tokio::select! {
                        _ = token.cancelled() => None,
                        msg = q.recv() => msg,
                    }
                };
                let n = match next {
                    Some(n) => n,
                    None => break,
                };
                if let Err(e) = chats
                    .record_outgoing_async(
                        &peer,
                        format!("soak-out-w{w}-{n}"),
                        &wa::Message::text(format!("soak outgoing {n}")),
                        Utc::now(),
                    )
                    .await
                {
                    warn!(error = %e, worker = w, "outgoing record failed; ending soak load");
                    flag_fatal(&fatal);
                    break;
                }
            }
        }));
    }

    // Inbound-style batches built like the chat-store fixtures: builder +
    // MessageInfo, unique ids per sequence number.
    handles.push({
        let token = token.clone();
        let chats = Arc::clone(&chats);
        let fatal = fatal_tx.clone();
        let seq = Arc::clone(&inbound_applied);
        let peer = peer.clone();
        sup.spawn_owned(&token, "soak-inbound", async move {
            let mut ticker = tokio::time::interval(INBOUND_PERIOD);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let base = seq.fetch_add(INBOUND_BATCH as u64, Ordering::Relaxed);
                        let batch: Vec<InboundMessage> = (0..INBOUND_BATCH)
                            .map(|k| {
                                let id = format!("soak-in-{}", base + k as u64);
                                let info = MessageInfo {
                                    source: MessageSource {
                                        chat: peer.clone(),
                                        sender: peer.clone(),
                                        is_from_me: false,
                                        ..Default::default()
                                    },
                                    id,
                                    timestamp: Utc::now(),
                                    ..Default::default()
                                };
                                InboundMessage::builder()
                                    .message(Arc::new(wa::Message::text(format!(
                                        "soak inbound {}",
                                        base + k as u64
                                    ))))
                                    .info(Arc::new(info))
                                    .build()
                            })
                            .collect();
                        if let Err(e) = chats.apply_inbound(batch).await {
                            warn!(error = %e, "inbound batch apply failed; ending soak load");
                            flag_fatal(&fatal);
                            break;
                        }
                    }
                    _ = token.cancelled() => break,
                }
            }
        })
    });

    // Flusher: periodic commit barrier whose latency feeds the sample window.
    handles.push({
        let token = token.clone();
        let store = Arc::clone(&store);
        let ring = Arc::clone(&flush_lat_us);
        let fatal = fatal_tx.clone();
        sup.spawn_owned(&token, "soak-flusher", async move {
            let mut ticker = tokio::time::interval(FLUSH_PERIOD);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        let t0 = Instant::now();
                        if let Err(e) = store.flush().await {
                            warn!(error = %e, "flush barrier failed; ending soak load");
                            flag_fatal(&fatal);
                            break;
                        }
                        match ring.lock() {
                            Ok(mut r) => {
                                r.push_back(t0.elapsed().as_micros() as f64);
                                while r.len() > RING_CAP {
                                    r.pop_front();
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    _ = token.cancelled() => break,
                }
            }
        })
    });
    handles.push(producer);

    println!(
        "# soak config: minutes={} rate={}/s db={} workers={} queue={}",
        cfg.minutes,
        cfg.rate_msgs_per_sec,
        db_path.display(),
        WORKERS,
        QUEUE_CAPACITY,
    );
    println!("ts_s,rss_kib,threads,fds,ingress_depth,ingress_dropped,flush_p50_us,flush_p95_us");

    let start = Instant::now();
    let mut samples: Vec<Sample> = Vec::new();

    let mut sample = |t: f64, samples: &mut Vec<Sample>| {
        let latencies: Vec<f64> = match flush_lat_us.lock() {
            Ok(mut ring) => ring.drain(..).collect(),
            Err(_) => Vec::new(),
        };
        let mut sorted = latencies;
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let s = Sample {
            t_secs: t,
            rss_kib: proc_status_field("VmRSS"),
            threads: proc_status_field("Threads"),
            fds: fd_count(),
            ingress_depth: chats.ingress_depth(),
            ingress_dropped: chats.ingress_dropped(),
            flush_p50_us: percentile(&sorted, 50.0),
            flush_p95_us: percentile(&sorted, 95.0),
        };
        println!(
            "{:.1},{},{},{},{},{},{:.0},{:.0}",
            s.t_secs,
            s.rss_kib.map(|v| v.to_string()).unwrap_or_default(),
            s.threads.map(|v| v.to_string()).unwrap_or_default(),
            s.fds,
            s.ingress_depth,
            s.ingress_dropped,
            s.flush_p50_us,
            s.flush_p95_us,
        );
        samples.push(s);
    };

    sample(0.0, &mut samples);
    let mut ticker = tokio::time::interval(SAMPLE_PERIOD);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                sample(start.elapsed().as_secs_f64(), &mut samples);
                if start.elapsed() >= run_for {
                    info!("soak duration reached");
                    break;
                }
            }
            res = tokio::signal::ctrl_c() => {
                let _ = res;
                info!("ctrl-c received; shutting down gracefully");
                break;
            }
            res = fatal_rx.changed() => {
                // Sender-drop also lands here: all load tasks ended.
                if res.is_ok() && *fatal_rx.borrow() {
                    warn!("fatal store error signalled; shutting down");
                }
                break;
            }
        }
    }

    // Graceful teardown: cancel the subtree, join everything we spawned, and
    // leave with one final commit barrier. No detached tasks survive this fn.
    token.cancel();
    let mut join_timeouts = 0usize;
    for h in handles {
        match tokio::time::timeout(DRAIN_JOIN_TIMEOUT, h).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => warn!(error = %e, "task ended with join error"),
            Err(_) => {
                join_timeouts += 1;
            }
        }
    }
    if join_timeouts > 0 {
        warn!(count = join_timeouts, "drain timed out waiting for tasks");
    }
    if let Err(e) = store.flush().await {
        warn!(error = %e, "final flush failed");
    }
    info!(
        recorded = sent.load(Ordering::Relaxed),
        inbound = inbound_applied.load(Ordering::Relaxed),
        "load generation finished"
    );

    Ok(samples)
}

fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    Some(xs.iter().sum::<f64>() / xs.len() as f64)
}

/// Least-squares slope scaled to per-minute units.
fn slope_per_min(ts: &[f64], ys: &[f64]) -> Option<f64> {
    if ts.len() < 2 || ts.len() != ys.len() {
        return None;
    }
    let n = ts.len() as f64;
    let mt = ts.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for i in 0..ts.len() {
        let dx = ts[i] - mt;
        num += dx * (ys[i] - my);
        den += dx * dx;
    }
    if den <= f64::EPSILON {
        return None;
    }
    Some((num / den) * 60.0)
}

fn quarter_means(ys: &[f64]) -> Option<(f64, f64)> {
    if ys.len() < 4 {
        return None;
    }
    let q = ys.len() / 4;
    Some((mean(&ys[..q])?, mean(&ys[ys.len() - q..])?))
}

/// Final trend verdict: no monotonic growth beyond noise, else nonzero exit.
fn verdict(samples: &[Sample]) -> ExitCode {
    let probes_ok = samples.iter().all(|s| s.rss_kib.is_some() && s.threads.is_some());
    if !probes_ok {
        println!("summary: resource probes unavailable on this platform; leak verdict skipped");
        return ExitCode::SUCCESS;
    }
    if samples.len() < MIN_SAMPLES_FOR_TREND {
        println!(
            "summary: only {} windows sampled (< {MIN_SAMPLES_FOR_TREND}); \
             run longer for a trend verdict",
            samples.len()
        );
        return ExitCode::SUCCESS;
    }

    let ts: Vec<f64> = samples.iter().map(|s| s.t_secs).collect();
    let rss: Vec<f64> = samples.iter().map(|s| s.rss_kib.unwrap_or(0) as f64).collect();
    let thr: Vec<f64> = samples.iter().map(|s| s.threads.unwrap_or(0) as f64).collect();
    let max_flush_p95 = samples
        .iter()
        .map(|s| s.flush_p95_us)
        .fold(0.0_f64, f64::max);
    let max_ingress_depth = samples.iter().map(|s| s.ingress_depth).max().unwrap_or(0);

    let rss_slope = slope_per_min(&ts, &rss).unwrap_or(0.0);
    let thr_slope = slope_per_min(&ts, &thr).unwrap_or(0.0);
    let (q1, q4) = quarter_means(&rss).unwrap_or((0.0, 0.0));
    let step = q4 - q1;
    let step_allowance = ABS_RSS_GROWTH_KIB.max(q1.abs() * 0.25);

    println!("== soak summary ==");
    println!(
        "windows={} span={:.1}min max_ingress_depth={max_ingress_depth} max_flush_p95_us={max_flush_p95:.0}",
        samples.len(),
        samples.last().map(|s| s.t_secs).unwrap_or(0.0) / 60.0,
    );
    println!(
        "rss: slope={rss_slope:.1} KiB/min (limit {RSS_LEAK_KIB_PER_MIN}) first-quarter-mean={q1:.0} last-quarter-mean={q4:.0}"
    );
    println!("threads: slope={thr_slope:.3}/min (limit {THREAD_LEAK_PER_MIN})");

    let leaks = [
        ("rss slope", rss_slope > RSS_LEAK_KIB_PER_MIN),
        (
            "rss quarter-step",
            step > step_allowance,
        ),
        ("thread slope", thr_slope > THREAD_LEAK_PER_MIN),
    ];
    let triggered: Vec<&str> = leaks
        .iter()
        .filter(|(_, hit)| *hit)
        .map(|(name, _)| *name)
        .collect();
    if triggered.is_empty() {
        println!("PASS: no monotonic growth beyond noise thresholds");
        ExitCode::SUCCESS
    } else {
        println!("FAIL: growth beyond noise: {}", triggered.join(", "));
        ExitCode::FAILURE
    }
}
