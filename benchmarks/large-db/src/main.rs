//! Large-database benchmark for the wasabi account store: generates a
//! charter-grade dataset (10k chats x ~100 messages) and measures the
//! read paths real UI surfaces hit, with wall-clock medians over repeated
//! iterations.
//!
//! Configuration is env-only: MODE=gen|bench|both (default both), GEN_ROWS
//! (default 1000000), GEN_DB=<path to sqlite file, required>. Run in release
//! mode; generation is fsync-bound, benches are latency-bound.

mod gen;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, bail};
use diesel::RunQueryDsl;
use wasabi_domain as domain;
use wasabi_repository::search::SearchService;
use wasabi_repository::{AccountStore, StoreTuning};

const CHAT_PAGE: usize = 100;
const MSG_PAGE: usize = 50;
const WARMUPS: usize = 3;
const ITERS: usize = 25;

struct OpStat {
    key: &'static str,
    label: &'static str,
    median_ns: u128,
    p95_ns: u128,
}

async fn bench_op<T, F, Fut>(key: &'static str, label: &'static str, mut op: F) -> anyhow::Result<OpStat>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, domain::ServiceError>>,
{
    // Warmups settle the SQLite page cache so samples measure steady state.
    for _ in 0..WARMUPS {
        op().await?;
    }
    let mut samples = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let t = Instant::now();
        op().await?;
        samples.push(t.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let n = samples.len();
    let median_ns = if n % 2 == 1 {
        samples[n / 2]
    } else {
        (samples[n / 2 - 1] + samples[n / 2]) / 2
    };
    let p95_idx = ((n * 95) / 100).saturating_sub(1);
    let p95_ns = samples[p95_idx];
    Ok(OpStat {
        key,
        label,
        median_ns,
        p95_ns,
    })
}

fn fmt_ns(ns: u128) -> String {
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

fn us(ns: u128) -> String {
    format!("{:.1}", ns as f64 / 1e3)
}

fn cursor_of(c: &domain::ChatSummary) -> domain::page::ChatPageCursor {
    domain::page::ChatPageCursor {
        pinned_at_ms: c.pinned_at_ms,
        last_activity_ms: c.last_activity_ms,
        chat: c.id.clone(),
    }
}

/// Walk the chat list once collecting page-tail cursors, then park at the
/// ~80th percentile — a deep-cursor page must cost a seek, not a fresh scan.
async fn deep_chat_cursor(store: &AccountStore) -> anyhow::Result<domain::page::ChatPageCursor> {
    let mut cursors: Vec<domain::page::ChatPageCursor> = Vec::new();
    let mut after: Option<domain::page::ChatPageCursor> = None;
    loop {
        let page = store.chat_page(false, after.take(), CHAT_PAGE).await?;
        let Some(last) = page.last() else { break };
        cursors.push(cursor_of(last));
        if page.len() < CHAT_PAGE {
            break;
        }
        after = cursors.last().cloned();
    }
    let at = cursors.len() * 4 / 5;
    cursors
        .into_iter()
        .nth(at)
        .context("chat list too short for a deep cursor")
}

#[derive(diesel::QueryableByName)]
struct PageCountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    page_count: i64,
}

#[derive(diesel::QueryableByName)]
struct PageSizeRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    page_size: i64,
}

fn sql_err(e: diesel::result::Error) -> wacore::store::error::StoreError {
    wacore::store::error::StoreError::Database(Box::new(e))
}

async fn run_gen(db: &Path, rows: u64) -> anyhow::Result<()> {
    // Refuse to pile duplicate fixtures onto an existing dataset; regenerating
    // means deleting the file first.
    if db.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        bail!("{} already exists; remove it before generating", db.display());
    }
    let store = AccountStore::open(db, &StoreTuning::default())
        .await
        .context("open account store for generation")?;
    let report = gen::generate(&store, rows).await?;
    tracing::info!(
        rows = report.rows,
        elapsed_s = report.elapsed.as_secs_f64(),
        rate_rows_s = (report.rows as f64 / report.elapsed.as_secs_f64()) as u64,
        "generation complete"
    );
    Ok(())
}

async fn run_bench(db: &Path) -> anyhow::Result<()> {
    let store = AccountStore::open(db, &StoreTuning::default())
        .await
        .context("open account store for benching")?;

    // Cursor discovery happens once, untimed.
    let first_page = store.chat_page(false, None, CHAT_PAGE).await.context("chat page 0")?;
    let newest = first_page
        .iter()
        .max_by_key(|c| c.last_activity_ms)
        .context("empty database; run gen mode first")?
        .clone();
    let deep = deep_chat_cursor(&store).await?;

    let newest_chat = newest.id.as_str().to_owned();
    let head_msg_page = store
        .message_page(&newest_chat, None, MSG_PAGE)
        .await
        .context("message head page")?;
    let older_cursor = head_msg_page
        .next_before
        .context("newest chat too short for an older page")?;
    drop(head_msg_page);

    let search = SearchService::new(store.chats().clone());

    let stats = vec![
        bench_op("chat_page", "chat_page(None, 100)", || {
            store.chat_page(false, None, CHAT_PAGE)
        })
        .await?,
        bench_op("chat_page_deep", "chat_page(deep@80%, 100)", || {
            store.chat_page(false, Some(deep.clone()), CHAT_PAGE)
        })
        .await?,
        bench_op("message_page", "message_page(newest, None, 50)", || {
            store.message_page(&newest_chat, None, MSG_PAGE)
        })
        .await?,
        bench_op("load_older", "message_page(cursor, 50)", || {
            store.message_page(&newest_chat, Some(older_cursor), MSG_PAGE)
        })
        .await?,
        bench_op("search", "search(common term, page 0)", || {
            search.search(gen::SEARCH_TERM, None, 0)
        })
        .await?,
    ];

    println!(
        "{:<30}{:>12}{:>12}{:>8}",
        "operation", "p50", "p95", "iters"
    );
    for s in &stats {
        println!(
            "{:<30}{:>12}{:>12}{:>8}",
            s.label,
            fmt_ns(s.median_ns),
            fmt_ns(s.p95_ns),
            ITERS
        );
    }

    // File footprint: bytes on disk plus the PRAGMA view of the same thing.
    let db_bytes = std::fs::metadata(db).map(|m| m.len()).unwrap_or(0);
    println!(
        "{:<30}{:>12}",
        "db size",
        humansize_bytes(db_bytes)
    );

    let shared = store.shared_db();
    // Pragmas are best-effort: their absence must not sink a bench run.
    let page_count = shared
        .run(|conn| {
            diesel::sql_query("PRAGMA page_count")
                .load::<PageCountRow>(conn)
                .map_err(sql_err)
        })
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .map(|r| r.page_count);
    let page_size = shared
        .run(|conn| {
            diesel::sql_query("PRAGMA page_size")
                .load::<PageSizeRow>(conn)
                .map_err(sql_err)
        })
        .await
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .map(|r| r.page_size);
    match (page_count, page_size) {
        (Some(count), Some(size)) => {
            println!("{:<30}{:>12} x {} B", "sqlite pages", count, size);
        }
        _ => tracing::warn!("PRAGMA page_count/page_size unavailable; skipping"),
    }

    // Machine-readable tail for CI tracking.
    let mut parts: Vec<String> = stats
        .iter()
        .flat_map(|s| {
            [
                format!("\"{}_p50_us\":{}", s.key, us(s.median_ns)),
                format!("\"{}_p95_us\":{}", s.key, us(s.p95_ns)),
            ]
        })
        .collect();
    parts.push(format!("\"db_bytes\":{db_bytes}"));
    if let (Some(count), Some(size)) = (page_count, page_size) {
        parts.push(format!("\"page_count\":{count}"));
        parts.push(format!("\"page_size\":{size}"));
    }
    println!("SUMMARY {{{}}}", parts.join(","));

    Ok(())
}

fn humansize_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

enum Mode {
    Gen,
    Bench,
    Both,
}

fn parse_mode(raw: &str) -> anyhow::Result<Mode> {
    match raw.to_ascii_lowercase().as_str() {
        "gen" => Ok(Mode::Gen),
        "bench" => Ok(Mode::Bench),
        "both" => Ok(Mode::Both),
        other => bail!("MODE must be gen|bench|both, got {other:?}"),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mode = match std::env::var("MODE") {
        Ok(raw) => parse_mode(&raw)?,
        Err(_) => Mode::Both,
    };
    let total_rows = match std::env::var("GEN_ROWS") {
        Ok(raw) => raw.parse::<u64>().context("GEN_ROWS must be an integer")?,
        Err(_) => 1_000_000,
    };
    let db: PathBuf = std::env::var("GEN_DB")
        .context("GEN_DB (database file path) is required")?
        .into();

    match mode {
        Mode::Gen => run_gen(&db, total_rows).await?,
        Mode::Bench => run_bench(&db).await?,
        Mode::Both => {
            run_gen(&db, total_rows).await?;
            run_bench(&db).await?;
        }
    }
    Ok(())
}
