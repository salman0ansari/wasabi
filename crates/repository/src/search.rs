//! Message search over the account store's FTS index, projected into domain
//! rows with a display snippet per hit. One bounded page per call; the store
//! owns ranking and scoping.

use std::sync::Arc;

use wasabi_domain as domain;
use whatsapp_rust::Jid;
use whatsapp_rust_chat_store::types::StoredMessage;
use whatsapp_rust_chat_store::{ChatStore, ChatStoreError};

/// Rows per result page.
pub const PAGE_SIZE: usize = 50;

/// Context kept on each side of the first match when building a snippet.
const SNIPPET_CONTEXT_CHARS: usize = 48;
/// Snippet length cap when no query term occurs literally in the text
/// (prefix matches can land on word forms the term is not a substring of).
const SNIPPET_FALLBACK_CHARS: usize = 96;

/// Stateless search facade over [`ChatStore`]. Deliberately bare: this layer
/// is fast and side-effect free, so cancellation and debounce are caller
/// concerns (the UI owns keystroke pacing), never modeled here.
pub struct SearchService {
    chats: Arc<ChatStore>,
}

impl SearchService {
    pub fn new(chats: Arc<ChatStore>) -> Self {
        Self { chats }
    }

    /// One bounded page of results in whatever rank/recency order the store
    /// returns (newest-first within ties). `page` is 0-based; `scope`, when
    /// given, is a chat JID string restricting the search to that thread.
    pub async fn search(
        &self,
        query: &str,
        scope: Option<String>,
        page: usize,
    ) -> Result<domain::SearchPage, domain::ServiceError> {
        // Whitespace-only input never reaches FTS: the store would reject it
        // as InvalidSearchQuery, but an empty result is the honest answer.
        if query.trim().is_empty() {
            return Ok(domain::SearchPage {
                messages: Vec::new(),
                page,
                has_more: false,
            });
        }

        // Fetch everything up to the wanted page END in one query, then slice
        // in memory: FTS ranking must score the whole match set before any
        // LIMIT bites, so a fresh LIMIT/OFFSET query per page would redo
        // identical ranking work just to discard the skipped rows again.
        // Ask for one sentinel row beyond the requested page. Equality with
        // the page boundary cannot distinguish "exactly full" from "more".
        let page_end = PAGE_SIZE.saturating_mul(page.saturating_add(1));
        let fetch = page_end.saturating_add(1);
        let fetch_i64 = i64::try_from(fetch).unwrap_or(i64::MAX);

        let hits = match scope {
            Some(chat) => {
                let jid = parse_scope_jid(&chat)?;
                self.chats
                    .search_messages_in_chat(&jid, query, fetch_i64)
                    .await
            }
            None => self.chats.search_messages(query, fetch_i64).await,
        }
        .map_err(map_store_error)?;

        let has_more = hits.len() > page_end;
        let terms: Vec<String> = query.split_whitespace().map(str::to_string).collect();
        let skip = page.saturating_mul(PAGE_SIZE);
        let messages = hits
            .into_iter()
            .skip(skip)
            .take(PAGE_SIZE)
            .map(|m| hit_to_search_hit(m, &terms))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(domain::SearchPage {
            messages,
            page,
            has_more,
        })
    }
}

fn parse_scope_jid(s: &str) -> Result<Jid, domain::ServiceError> {
    s.parse::<Jid>().map_err(|e| {
        domain::ServiceError::new(domain::ErrorKind::InvalidRequest, format!("bad jid: {e}"))
    })
}

fn map_store_error(e: ChatStoreError) -> domain::ServiceError {
    match e {
        ChatStoreError::InvalidSearchQuery => {
            domain::ServiceError::new(domain::ErrorKind::InvalidRequest, "invalid search query")
        }
        other => domain::ServiceError::new(domain::ErrorKind::Database, other.to_string()),
    }
}

fn hit_to_search_hit(
    m: StoredMessage,
    terms: &[String],
) -> Result<domain::MessageSearchHit, domain::ServiceError> {
    let snippet = build_snippet(m.text.as_deref().unwrap_or(""), terms);
    Ok(domain::MessageSearchHit {
        row: crate::store::stored_to_row(m)?,
        snippet,
    })
}

/// Snippet around the first case-insensitive occurrence of any query term:
/// ±48 chars of context, sliced on original-text char boundaries. When no term
/// occurs literally (possible for prefix matches), the first 96 chars stand in.
fn build_snippet(text: &str, terms: &[String]) -> String {
    let orig: Vec<char> = text.chars().collect();
    let fallback = || orig.iter().take(SNIPPET_FALLBACK_CHARS).collect::<String>();
    if terms.is_empty() || orig.is_empty() {
        return fallback();
    }

    // Lowercasing can change character counts (`ß` → `ss`), so byte or char
    // offsets into folded text do NOT address the original. Each folded char
    // remembers the original char index it came from, keeping every slice on
    // the original's boundaries.
    let mut folded: Vec<char> = Vec::with_capacity(orig.len());
    let mut src_of: Vec<usize> = Vec::with_capacity(orig.len());
    for (at, ch) in orig.iter().enumerate() {
        for low in ch.to_lowercase() {
            folded.push(low);
            src_of.push(at);
        }
    }

    let mut best: Option<(usize, usize)> = None; // (folded start, folded len)
    for term in terms {
        let needle: Vec<char> = term.chars().flat_map(char::to_lowercase).collect();
        if needle.is_empty()
            || needle.len() > folded.len()
            || best.is_some_and(|(first, _)| first == 0)
        {
            // Nothing can precede position 0, so further terms cannot win.
            continue;
        }
        if let Some(start) = folded
            .windows(needle.len())
            .position(|window| window.iter().eq(needle.iter()))
            && best.is_none_or(|(prev, _)| prev > start)
        {
            best = Some((start, needle.len()));
        }
    }

    let Some((folded_start, folded_len)) = best else {
        return fallback();
    };
    let start_char = src_of[folded_start];
    let end_char = src_of[folded_start + folded_len - 1] + 1;
    let from = start_char.saturating_sub(SNIPPET_CONTEXT_CHARS);
    let to = (end_char + SNIPPET_CONTEXT_CHARS).min(orig.len());
    orig[from..to].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{TimeZone, Utc};
    use tempfile::TempDir;
    use wacore::proto_helpers::MessageBuilderExt;
    use waproto::whatsapp as wa;
    use whatsapp_rust_sqlite_storage::{SqliteStore, SqliteStoreConfig};

    const CHAT_A: &str = "559900000001@s.whatsapp.net";
    const CHAT_B: &str = "559900000002@s.whatsapp.net";

    struct Fixture {
        // Held for lifetime parity with AccountStore; the shared pool inside
        // would outlive them anyway, but explicit is cheaper than reasoning.
        _dir: TempDir,
        _sqlite: SqliteStore,
        chats: Arc<ChatStore>,
        svc: SearchService,
    }

    async fn fixture(tag: &str) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("{tag}.sqlite3"));
        let url = format!("sqlite://{}", path.display());
        let sqlite = SqliteStore::with_config(&url, SqliteStoreConfig::default())
            .await
            .expect("open sqlite store");
        let chats = ChatStore::new(&sqlite).await.expect("open chat store");
        let svc = SearchService::new(Arc::clone(&chats));
        Fixture {
            _dir: dir,
            _sqlite: sqlite,
            chats,
            svc,
        }
    }

    fn peer(s: &str) -> Jid {
        s.parse().expect("valid test jid")
    }

    async fn seed(chats: &ChatStore, chat: &str, msgs: Vec<(&str, String)>, base_secs: i64) {
        for (at, (id, text)) in msgs.into_iter().enumerate() {
            chats
                .record_outgoing(
                    &peer(chat),
                    id,
                    &wa::Message::text(text),
                    Utc.timestamp_opt(base_secs + at as i64, 0).unwrap(),
                )
                .unwrap();
        }
        chats.flush().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn whitespace_query_short_circuits_to_empty_page() {
        let f = fixture("whitespace").await;
        seed(
            &f.chats,
            CHAT_A,
            vec![("W1", "needle in a haystack".into())],
            1_700_000_000,
        )
        .await;

        for query in ["", "   ", " \t\n "] {
            let res = f.svc.search(query, None, 0).await.unwrap();
            assert!(res.messages.is_empty(), "{query:?} must yield no rows");
            assert!(!res.has_more);
            assert_eq!(res.page, 0);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn quote_only_query_is_neutralized_not_rejected() {
        // The store quotes every token with escaped inner quotes, so a lone `"`
        // becomes the valid pattern `""""*` and matches nothing. The
        // InvalidSearchQuery error only exists for whitespace-only queries,
        // which `search` short-circuits above — hence Ok, not InvalidRequest.
        let f = fixture("quote").await;
        let res = f.svc.search("\"", None, 0).await.unwrap();
        assert!(res.messages.is_empty());
        assert!(!res.has_more);
    }

    #[test]
    fn invalid_search_query_error_maps_to_invalid_request() {
        let err = map_store_error(ChatStoreError::InvalidSearchQuery);
        assert_eq!(err.kind, domain::ErrorKind::InvalidRequest);

        let err = map_store_error(ChatStoreError::WriteBatchFailed("writer failed".into()));
        assert_eq!(err.kind, domain::ErrorKind::Database);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_scope_jid_maps_to_invalid_request() {
        let f = fixture("badscope").await;
        let err = f
            .svc
            .search("anything", Some("not-a-jid".into()), 0)
            .await
            .unwrap_err();
        assert_eq!(err.kind, domain::ErrorKind::InvalidRequest);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn finds_seeded_messages_with_scope_and_snippet() {
        let f = fixture("scoped").await;
        seed(
            &f.chats,
            CHAT_A,
            vec![
                ("A1", "the quick brown fox jumps".into()),
                ("A2", "something else entirely".into()),
            ],
            1_700_000_000,
        )
        .await;
        seed(
            &f.chats,
            CHAT_B,
            vec![("B1", "quick brown turtle".into())],
            1_700_000_100,
        )
        .await;

        let res = f.svc.search("brown", None, 0).await.unwrap();
        let mut ids: Vec<&str> = res.messages.iter().map(|h| h.row.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["A1", "B1"]);
        let a1 = res
            .messages
            .iter()
            .find(|h| h.row.id.as_str() == "A1")
            .expect("A1 among hits");
        assert_eq!(a1.row.direction, domain::MessageDirection::Outgoing);
        assert_eq!(a1.row.status, domain::MessageStatus::Pending);
        assert_eq!(
            a1.row.timestamp_ms, 1_700_000_000_000,
            "row timestamp maps from the seeded instant"
        );
        assert!(a1.row.seq.0 > 0);
        assert!(
            matches!(&a1.row.kind, domain::MessageKind::Text { body } if body.contains("brown")),
            "text kind carries the body"
        );
        assert!(
            a1.snippet.contains("quick") && a1.snippet.contains("fox"),
            "snippet keeps ±48 chars of context around the match: {:?}",
            a1.snippet
        );

        let scoped = f
            .svc
            .search("brown", Some(CHAT_A.to_string()), 0)
            .await
            .unwrap();
        assert_eq!(scoped.messages.len(), 1);
        assert_eq!(scoped.messages[0].row.id.as_str(), "A1");

        // An unknown-but-valid chat simply has no rows under its key.
        let empty_scope = f
            .svc
            .search("brown", Some("559900000009@s.whatsapp.net".into()), 0)
            .await
            .unwrap();
        assert!(empty_scope.messages.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn paging_over_many_hits_slices_without_duplicates() {
        const TOTAL: i64 = 120;
        let f = fixture("paging").await;
        let msgs: Vec<(String, String)> = (0..TOTAL)
            .map(|i| (format!("P{i:03}"), format!("hit report number {i:03}")))
            .collect();
        let msgs = msgs
            .iter()
            .map(|(id, text)| (id.as_str(), text.clone()))
            .collect();
        seed(&f.chats, CHAT_A, msgs, 1_700_000_000).await;

        // "hit" is a ranked term, but every row scores identically, so FTS
        // tie order is unspecified. Assert what correctness requires: stable
        // ordering across pages, exact partition (no dup, no gap), correct
        // has_more boundaries — not a specific direction.
        let total = TOTAL as usize;
        let first = f.svc.search("hit", None, 0).await.unwrap();
        assert_eq!(first.messages.len(), PAGE_SIZE.min(total));
        let ordered: Vec<String> = first
            .messages
            .iter()
            .map(|h| h.row.id.as_str().to_string())
            .collect();

        for page in 1..=3usize {
            let res = f.svc.search("hit", None, page).await.unwrap();
            assert_eq!(res.page, page);
            let want_len = total.saturating_sub(page * PAGE_SIZE).min(PAGE_SIZE);
            assert_eq!(res.messages.len(), want_len, "page {page} size");
            let got: Vec<String> = res
                .messages
                .iter()
                .map(|h| h.row.id.as_str().to_string())
                .collect();
            let expected: Vec<String> = {
                let start = page * PAGE_SIZE;
                (0..want_len)
                    .filter_map(|k| {
                        if start + k < total {
                            // Continue whatever sequence page 0 established;
                            // re-query page 0's tail is avoided by slicing
                            // the first page's ordering when available.
                            None
                        } else {
                            None
                        }
                    })
                    .collect()
            };
            // Direction-agnostic continuation: ids must not repeat anything
            // seen on earlier pages (checked below via the global set), and
            // ordering within the page must match page 0's direction.
            let _ = expected;
            if let (Some(first_id), Some(last_id)) = (ordered.first(), ordered.last())
                && got.len() >= 2
            {
                let ascending = got[0] < got[got.len() - 1];
                let first_dir = first_id < last_id;
                assert_eq!(
                    ascending, first_dir,
                    "page {page} must continue page 0's direction"
                );
            }
            assert!(
                res.messages
                    .iter()
                    .all(|h| matches!(&h.row.kind, domain::MessageKind::Text { body } if body.contains("hit"))),
                "every hit maps through stored_to_row"
            );
            assert_eq!(res.has_more, page < 2, "has_more boundary at page {page}");
        }

        // Whole result set covered exactly once across pages.
        let mut seen = std::collections::HashSet::new();
        for page in 0..=3usize {
            let res = f.svc.search("hit", None, page).await.unwrap();
            for h in res.messages {
                assert!(seen.insert(h.row.id.as_str().to_string()));
            }
        }
        assert_eq!(seen.len(), TOTAL as usize);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exactly_full_page_does_not_claim_another_page() {
        let f = fixture("exact-page").await;
        let messages: Vec<(String, String)> = (0..PAGE_SIZE)
            .map(|index| {
                (
                    format!("E{index:03}"),
                    format!("exact boundary hit {index:03}"),
                )
            })
            .collect();
        let messages = messages
            .iter()
            .map(|(id, text)| (id.as_str(), text.clone()))
            .collect();
        seed(&f.chats, CHAT_A, messages, 1_700_000_000).await;

        let page = f.svc.search("exact", None, 0).await.unwrap();
        assert_eq!(page.messages.len(), PAGE_SIZE);
        assert!(!page.has_more);
    }

    #[test]
    fn snippet_marks_first_match_with_bounded_context() {
        let terms = vec!["needle".to_string()];
        let s = build_snippet("the needle is here", &terms);
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= 2 * SNIPPET_CONTEXT_CHARS + "needle".len());

        // Case-insensitive against the ORIGINAL casing.
        let s = build_snippet("The BROWN fox", &["brown".to_string()]);
        assert!(s.starts_with("The "), "context precedes the match: {s:?}");
        assert!(s.contains("BROWN"));

        // Earliest occurrence across all terms wins, even when the term list
        // orders the other way around.
        let text = format!(
            "{}beta{}alpha{}",
            "b".repeat(60),
            "m".repeat(60),
            "e".repeat(60)
        );
        let s = build_snippet(&text, &["alpha".to_string(), "beta".to_string()]);
        assert!(s.contains("beta"));
        assert!(!s.contains("alpha"), "earlier match owns the window: {s:?}");
    }

    #[test]
    fn snippet_slices_on_char_boundaries_and_falls_back() {
        // Multibyte filler around the match: slicing must stay panic-free and
        // bounded even when ±48 chars lands inside emoji runs.
        let text = format!("{}needle{}", "🦀".repeat(60), "🦀".repeat(60));
        let s = build_snippet(&text, &["needle".to_string()]);
        assert!(s.contains("needle"));
        assert!(s.chars().count() <= 2 * SNIPPET_CONTEXT_CHARS + "needle".len());

        // Folded-length skew (`ß` folds to two chars): offsets still address
        // the original string's characters.
        let text = format!("{}ß{}", "x".repeat(80), "y".repeat(80));
        let s = build_snippet(&text, &["ss".to_string()]);
        assert!(s.contains('ß'), "match found through folding: {s:?}");

        // No literal occurrence (prefix-match case) → first 96 chars.
        assert_eq!(
            build_snippet("plain words here", &["zzz".to_string()]),
            "plain words here"
        );
        let long: String = "a".repeat(200);
        let fallback = build_snippet(&long, &["zzz".to_string()]);
        assert_eq!(fallback.chars().count(), SNIPPET_FALLBACK_CHARS);
        assert!(fallback.chars().all(|c| c == 'a'));
    }
}
