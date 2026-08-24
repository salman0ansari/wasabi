//! Root window entity: three-pane shell layout, degraded states, and every
//! long-lived bridge task (hydration, invalidations, session/QR watches).
//!
//! All background work runs through [`Context::spawn`], which hands each task
//! a weak handle; every wake-up re-checks `upgrade()` before touching state,
//! and stale async results are dropped via per-view generation counters.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, Context, FocusHandle, Focusable, Global, KeyBinding, ListAlignment,
    ListState, PathPromptOptions, Subscription, WeakEntity, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, VirtualListScrollHandle};

use crate::core_bridge::DesktopBackend;
use crate::state::chats::ChatFilter;
use crate::state::{ChatListModel, DeviceSettings, MessageWindowModel, SessionMirror, SettingsSection};
use crate::theme;
use crate::views::{chat_list, composer, conversation, pairing, right_panel, settings};
use wasabi_domain::{ChatKind, ChatScope, ConversationDetails};

gpui::actions!(wasabi_desktop, [FocusSearch, OpenSettings, CloseInfo]);

pub const MAIN_KEY_CONTEXT: &str = "Main";
const CHAT_PAGE_LIMIT: usize = 100;
const MESSAGE_PAGE_LIMIT: usize = 60;
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
const DRAFT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);
const TYPING_PAUSE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);
const TYPING_REFRESH_AFTER: std::time::Duration = std::time::Duration::from_secs(4);
const INCOMING_TYPING_TTL: std::time::Duration = std::time::Duration::from_secs(4);
/// Countdown label refresh interval.
const COUNTDOWN_TICK: std::time::Duration = std::time::Duration::from_secs(1);
const NOTIFICATION_DEDUPE_LIMIT: usize = 4096;

enum LoadedConversation {
    Newest(wasabi_domain::MessagePage),
    Context(wasabi_domain::MessageContext),
}

#[derive(Clone)]
pub(crate) enum MessageOverlay {
    Actions(wasabi_domain::MessageId),
    Confirm(wasabi_domain::MessageAction),
}

#[derive(Clone, Copy)]
pub(crate) enum SettingsOverlay {
    ClearMediaCache,
    Logout,
}

#[derive(Clone)]
pub(crate) enum SettingsFeedback {
    Success(String),
    Error(String),
}

#[derive(Clone)]
pub(crate) enum MediaDownloadUi {
    Downloading,
    Ready(std::path::PathBuf),
    Failed,
}

#[derive(Clone)]
pub(crate) struct TypingDisplay {
    pub state: wasabi_domain::TypingState,
    pub participant: Option<String>,
    generation: u64,
}

impl TypingDisplay {
    pub fn label(&self, group: bool) -> String {
        let action = match self.state {
            wasabi_domain::TypingState::Composing => "typing…",
            wasabi_domain::TypingState::RecordingAudio => "recording audio…",
            wasabi_domain::TypingState::Paused => return String::new(),
        };
        if group {
            self.participant
                .as_deref()
                .and_then(|participant| participant.split('@').next())
                .filter(|participant| !participant.is_empty())
                .map_or_else(|| action.to_string(), |participant| format!("{participant} is {action}"))
        } else {
            action.to_string()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavDestination {
    Chats,
    Settings,
}

impl NavDestination {
    fn chat_filter(self) -> Option<ChatFilter> {
        match self {
            Self::Chats => Some(ChatFilter::All),
            Self::Settings => None,
        }
    }

}

/// Startup-installed global so the window can reach the process bridge.
pub struct BridgeGlobal(pub Arc<dyn DesktopBackend>);

impl Global for BridgeGlobal {}

pub struct MainWindow {
    pub(crate) bridge: Arc<dyn DesktopBackend>,
    focus: FocusHandle,
    pub(crate) chats: ChatListModel,
    pub(crate) messages: MessageWindowModel,
    pub(crate) session: SessionMirror,
    pub(crate) typing: HashMap<String, TypingDisplay>,
    nav_destination: NavDestination,
    pub(crate) show_right_panel: bool,
    pub(crate) conversation_details: Option<ConversationDetails>,
    pub(crate) details_loading: bool,
    pub(crate) details_error: Option<String>,
    pub(crate) settings: DeviceSettings,
    pub(crate) settings_section: SettingsSection,
    pub(crate) settings_overlay: Option<SettingsOverlay>,
    pub(crate) settings_feedback: Option<SettingsFeedback>,
    pub(crate) media_cache_usage_bytes: Option<u64>,
    pub(crate) media_cache_loading: bool,
    pub(crate) logout_in_progress: bool,
    pub(crate) send_error: Option<String>,
    pub(crate) message_overlay: Option<MessageOverlay>,
    pub(crate) media_downloads:
        HashMap<(wasabi_domain::ChatId, wasabi_domain::MediaId), MediaDownloadUi>,
    pub(crate) staged_attachments: HashMap<String, wasabi_domain::StagedAttachment>,
    pub(crate) attachment_staging: HashSet<String>,
    pub(crate) attachment_sending: HashSet<String>,
    pub(crate) composer_input: gpui::Entity<InputState>,
    pub(crate) search_input: gpui::Entity<InputState>,
    pub(crate) phone_pair_input: gpui::Entity<InputState>,
    pub(crate) chat_scroll: VirtualListScrollHandle,
    pub(crate) msg_scroll: ListState,
    /// First visible timeline index observed on the last frame.
    pub(crate) first_visible: usize,
    /// Whether the last frame showed the newest end of the timeline.
    pub(crate) near_bottom: bool,
    pub(crate) pending_new_messages: usize,
    window_handle: AnyWindowHandle,
    window_active: bool,
    notification_started_at_ms: i64,
    notification_seen: HashSet<(String, String)>,
    notification_seen_order: VecDeque<(String, String)>,
    notifications: crate::notifications::NotificationDispatcher,
    draft_generations: HashMap<String, u64>,
    outbound_typing_generations: HashMap<String, u64>,
    outbound_typing_sent_at: HashMap<String, std::time::Instant>,
    chats_gen: AtomicU64,
    search_gen: AtomicU64,
    messages_gen: AtomicU64,
    details_gen: AtomicU64,
    qr_ticker_gen: AtomicU64,
    phone_pair_ticker_gen: AtomicU64,
    pairing_request_gen: AtomicU64,
    phone_pair_request_gen: AtomicU64,
    #[allow(dead_code)]
    subscriptions: Vec<Subscription>,
}

impl Focusable for MainWindow {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

pub fn key_bindings() -> Vec<KeyBinding> {
    // One binding per platform prefix: the keystroke parser at this rev
    // rejects the "cmd-k|ctrl-k" compound form.
    let mut bindings = vec![KeyBinding::new(
        "escape",
        CloseInfo,
        Some(MAIN_KEY_CONTEXT),
    )];
    if cfg!(target_os = "macos") {
        bindings.push(KeyBinding::new("cmd-k", FocusSearch, Some(MAIN_KEY_CONTEXT)));
        bindings.push(KeyBinding::new(
            "cmd-,",
            OpenSettings,
            Some(MAIN_KEY_CONTEXT),
        ));
    } else {
        bindings.push(KeyBinding::new("ctrl-k", FocusSearch, Some(MAIN_KEY_CONTEXT)));
        bindings.push(KeyBinding::new(
            "ctrl-,",
            OpenSettings,
            Some(MAIN_KEY_CONTEXT),
        ));
    }
    bindings
}

impl MainWindow {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let bridge = cx
            .try_global::<BridgeGlobal>()
            .expect("bridge global installed before window")
            .0
            .clone();

        let composer_input = composer::build_input(window, cx);
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search or start new chat"));
        let phone_pair_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Country code and phone number")
        });
        let (notification_click_tx, notification_click_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut this = Self {
            bridge,
            focus: cx.focus_handle(),
            chats: ChatListModel::new(),
            messages: MessageWindowModel::new(),
            session: SessionMirror::new(),
            typing: HashMap::new(),
            nav_destination: NavDestination::Chats,
            show_right_panel: false,
            conversation_details: None,
            details_loading: false,
            details_error: None,
            settings: DeviceSettings::load(),
            settings_section: SettingsSection::Chats,
            settings_overlay: None,
            settings_feedback: None,
            media_cache_usage_bytes: None,
            media_cache_loading: false,
            logout_in_progress: false,
            send_error: None,
            message_overlay: None,
            media_downloads: HashMap::new(),
            staged_attachments: HashMap::new(),
            attachment_staging: HashSet::new(),
            attachment_sending: HashSet::new(),
            composer_input,
            search_input,
            phone_pair_input,
            chat_scroll: VirtualListScrollHandle::new(),
            msg_scroll: ListState::new(0, ListAlignment::Bottom, px(800.0)),
            first_visible: 0,
            near_bottom: true,
            pending_new_messages: 0,
            window_handle: window.window_handle(),
            window_active: window.is_window_active(),
            notification_started_at_ms: chrono::Utc::now().timestamp_millis(),
            notification_seen: HashSet::new(),
            notification_seen_order: VecDeque::new(),
            notifications: crate::notifications::NotificationDispatcher::new(
                notification_click_tx,
            ),
            draft_generations: HashMap::new(),
            outbound_typing_generations: HashMap::new(),
            outbound_typing_sent_at: HashMap::new(),
            chats_gen: AtomicU64::new(0),
            search_gen: AtomicU64::new(0),
            messages_gen: AtomicU64::new(0),
            details_gen: AtomicU64::new(0),
            qr_ticker_gen: AtomicU64::new(0),
            phone_pair_ticker_gen: AtomicU64::new(0),
            pairing_request_gen: AtomicU64::new(0),
            phone_pair_request_gen: AtomicU64::new(0),
            subscriptions: Vec::new(),
        };
        let message_list = this.msg_scroll.clone();
        let main_window = cx.entity().downgrade();
        this.msg_scroll
            .set_scroll_handler(move |event, _window, cx| {
                let first_visible = event.visible_range.start;
                let near_bottom = event.is_following_tail
                    || event.visible_range.end >= message_list.item_count().saturating_sub(2);
                let main_window = main_window.clone();
                cx.defer(move |cx| {
                    let _ = main_window.update(cx, |this, _cx| {
                        this.first_visible = first_visible;
                        this.near_bottom = near_bottom;
                    });
                });
            });
        // Storage normally opens before the window appears; reflect a
        // not-yet-ready store as loading regardless of startup speed.
        this.chats.loading = !this.bridge.store_ready();

        let on_search_change = cx.subscribe_in(&this.search_input, window, {
            let search_input = this.search_input.clone();
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.chats.query = search_input.read(cx).value().to_string();
                    this.refresh_visible();
                    this.queue_search(cx);
                    cx.notify();
                }
            }
        });
        this.subscriptions.push(on_search_change);

        let on_composer_change = cx.subscribe_in(&this.composer_input, window, {
            move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.queue_draft_save(cx);
                    if this
                        .composer_input
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window)
                    {
                        this.queue_outbound_typing(cx);
                    }
                }
            }
        });
        this.subscriptions.push(on_composer_change);

        // Deterministic teardown mirrors the supervisor sequence: flush
        // durable boundaries first, then stop the session. The callback is
        // sync at this rev; the async body parks on a detached task.
        let on_quit = cx.on_app_quit(|this, _cx| {
            let bridge = Arc::clone(&this.bridge);
            let pending_draft = this.chats.selected.clone().map(|chat| {
                let body = this.composer_input.read(_cx).value().to_string();
                let staged_attachments = this
                    .staged_attachments
                    .get(&chat)
                    .map(|attachment| vec![attachment.transfer.as_str().to_string()])
                    .unwrap_or_default();
                let draft = (!body.trim().is_empty() || !staged_attachments.is_empty()).then(|| {
                    wasabi_domain::Draft {
                        body,
                        staged_attachments,
                        ..wasabi_domain::Draft::default()
                    }
                });
                (wasabi_domain::ChatId::new(chat), draft)
            });
            async move {
                if let Some((chat, draft)) = pending_draft {
                    let _ = bridge.save_draft(chat, draft).await;
                }
                let _ = bridge.flush_storage().await;
                let _ = bridge.stop_session().await;
            }
        });
        this.subscriptions.push(on_quit);

        #[cfg(debug_assertions)]
        let previewing = if let Ok(preview_mode) = std::env::var("WASABI_UI_PREVIEW") {
            this.install_preview(&preview_mode);
            true
        } else {
            false
        };
        #[cfg(not(debug_assertions))]
        let previewing = false;

        if !previewing {
            this.spawn_hydration(cx);
            this.spawn_invalidation_loop(cx);
            this.spawn_state_watch(cx);
            this.spawn_qr_watch(cx);
            this.spawn_typing_watch(cx);
            this.spawn_notification_click_watch(notification_click_rx, cx);
        }
        window.focus(&this.focus, cx);
        this
    }

    /// Deterministic debug-only surface for screenshot and visual-regression
    /// inspection. It never ships in release builds and never performs backend
    /// mutations. Use `WASABI_UI_PREVIEW=media` or `settings` with a debug
    /// binary.
    #[cfg(debug_assertions)]
    fn install_preview(&mut self, mode: &str) {
        let preview = crate::state::preview::media_preview();
        self.session.state = wasabi_core::state::SessionState::Connected;
        self.session.connected_once = true;
        self.chats.loading = false;
        self.chats.selected = Some(preview.chat.as_str().to_string());
        self.chats.chats = vec![preview.summary];
        self.messages.chat_id = Some(preview.chat.as_str().to_string());
        self.messages.anchor_newest(&preview.page);
        self.msg_scroll.reset(self.messages.items.len());
        self.msg_scroll.scroll_to_end();
        if mode == "media" {
            self.staged_attachments.insert(
                preview.chat.as_str().to_string(),
                wasabi_domain::StagedAttachment {
                    transfer: wasabi_domain::TransferId::new("preview-transfer"),
                    kind: wasabi_domain::AttachmentKind::Document,
                    display_name: "Wasabi product brief.pdf".to_string(),
                    mime_type: "application/pdf".to_string(),
                    bytes_total: 2_621_440,
                },
            );
        }
        self.typing.insert(
            preview.chat.as_str().to_string(),
            TypingDisplay {
                state: wasabi_domain::TypingState::Composing,
                participant: None,
                generation: 1,
            },
        );
        if mode == "timeline-large" {
            self.settings.text_scale = 150;
            self.msg_scroll.remeasure();
        }
        if mode == "timeline-pending" {
            self.pending_new_messages = 3;
            self.near_bottom = false;
        }
        if matches!(mode, "settings" | "settings-dark" | "account") {
            self.nav_destination = NavDestination::Settings;
            self.settings_section = if mode == "account" {
                SettingsSection::Account
            } else {
                SettingsSection::Storage
            };
            self.media_cache_usage_bytes = Some(187 * 1024 * 1024);
        }
    }

    // ---- User intents ------------------------------------------------------

    pub(crate) fn set_chat_filter(&mut self, filter: ChatFilter, cx: &mut Context<Self>) {
        self.chats.filter = filter;
        self.refresh_visible();
        cx.notify();
    }

    pub(crate) fn show_archived(&mut self, cx: &mut Context<Self>) {
        self.switch_chat_scope(ChatScope::Archived, cx);
    }

    pub(crate) fn show_active_chats(&mut self, cx: &mut Context<Self>) {
        self.switch_chat_scope(ChatScope::Active, cx);
    }

    fn queue_search(&mut self, cx: &mut Context<Self>) {
        let generation = self.search_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let query = self.chats.query.trim().to_string();
        if query.is_empty() {
            self.chats.clear_search();
            cx.notify();
            return;
        }
        self.chats.clear_search();
        self.chats.search_loading = true;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let current = this
                .update(cx, |this, _| {
                    this.search_gen.load(Ordering::Acquire) == generation
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let result = bridge.search_messages(query, None, 0).await;
            this.update(cx, |this, cx| {
                if this.search_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match result {
                    Ok(page) => this.chats.set_search_page(page),
                    Err(error) => this.chats.set_search_error(error),
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(crate) fn load_more_search(&mut self, cx: &mut Context<Self>) {
        if self.chats.search_loading || !self.chats.search_has_more {
            return;
        }
        let generation = self.search_gen.load(Ordering::Acquire);
        let query = self.chats.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let page = self.chats.search_page.saturating_add(1);
        self.chats.search_loading = true;
        self.chats.search_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.search_messages(query, None, page).await;
            this.update(cx, |this, cx| {
                if this.search_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match result {
                    Ok(page) => this.chats.append_search_page(page),
                    Err(error) => this.chats.set_search_error(error),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn switch_chat_scope(&mut self, scope: ChatScope, cx: &mut Context<Self>) {
        if self.chats.scope == scope {
            return;
        }
        self.chats.scope = scope;
        self.chats.filter = ChatFilter::All;
        self.chats.chats.clear();
        self.chats.visible_cache.clear();
        self.chats.selected = None;
        self.chats.loading = true;
        self.details_gen.fetch_add(1, Ordering::AcqRel);
        self.show_right_panel = false;
        self.message_overlay = None;
        self.conversation_details = None;
        self.details_loading = false;
        self.details_error = None;
        self.refresh_chats(cx);
    }

    fn select_nav(&mut self, destination: NavDestination, cx: &mut Context<Self>) {
        self.nav_destination = destination;
        if let Some(filter) = destination.chat_filter() {
            self.set_chat_filter(filter, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn select_chat(
        &mut self,
        chat_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chat(chat_id, None, window, cx);
    }

    pub(crate) fn open_search_result(
        &mut self,
        chat_id: String,
        message_id: wasabi_domain::MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_chat(chat_id, Some(message_id), window, cx);
    }

    fn open_chat(
        &mut self,
        chat_id: String,
        anchor: Option<wasabi_domain::MessageId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if anchor.is_none() && self.chats.selected.as_deref() == Some(chat_id.as_str()) {
            return;
        }
        if let Some(previous) = self.chats.selected.clone()
            && previous != chat_id
        {
            self.stop_outbound_typing(previous, cx);
        }
        // Bump first so any in-flight message load is discarded as stale.
        self.messages_gen.fetch_add(1, Ordering::AcqRel);
        self.details_gen.fetch_add(1, Ordering::AcqRel);
        self.chats.selected = Some(chat_id.clone());
        self.messages.reset_for_chat(&chat_id);
        self.msg_scroll.reset(0);
        self.show_right_panel = false;
        self.message_overlay = None;
        self.conversation_details = None;
        self.details_loading = false;
        self.details_error = None;
        self.first_visible = 0;
        self.pending_new_messages = 0;
        // An anchored search result starts in the middle of its context; do
        // not prefetch toward newest until the first rendered range actually
        // reaches that edge.
        self.near_bottom = anchor.is_none();
        let draft = self
            .chats
            .chats
            .iter()
            .find(|chat| chat.id.as_str() == chat_id)
            .and_then(|chat| chat.draft_preview.clone())
            .unwrap_or_default();
        self.composer_input
            .update(cx, |input, cx| input.set_value(draft, window, cx));
        let should_mark_read = window.is_window_active()
            && self.session.can_send()
            && self
                .chats
                .chats
                .iter()
                .find(|chat| chat.id.as_str() == chat_id)
                .is_some_and(|chat| chat.unread_count != 0);
        if should_mark_read {
            // The immutable chat identity is captured before the command is
            // dispatched. A rapid conversation switch cannot mark the next
            // chat read by mistake.
            self.perform_chat_action(
                wasabi_domain::ChatAction::MarkRead {
                    chat: wasabi_domain::ChatId::new(chat_id.clone()),
                    read: true,
                },
                cx,
            );
        }
        let generation = self.next_messages_gen();

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = match anchor.as_ref() {
                Some(message) => bridge
                    .load_message_context(
                        chat_id.clone(),
                        message.clone(),
                        MESSAGE_PAGE_LIMIT / 2,
                        MESSAGE_PAGE_LIMIT / 2,
                    )
                    .await
                    .map(LoadedConversation::Context),
                None => bridge
                    .load_message_page(&chat_id, None, MESSAGE_PAGE_LIMIT)
                    .await
                    .map(LoadedConversation::Newest),
            };
            this.update(cx, |this, cx| {
                if this.messages_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match result {
                    Ok(LoadedConversation::Newest(page)) => {
                        this.messages.anchor_newest(&page);
                        this.msg_scroll.reset(this.messages.items.len());
                        this.msg_scroll.scroll_to_end();
                    }
                    Ok(LoadedConversation::Context(context)) => {
                        let anchor = context.anchor.clone();
                        this.messages.anchor_context(&context);
                        this.msg_scroll.reset(this.messages.items.len());
                        if let Some(index) = this.messages.timeline_index_for_message(&anchor) {
                            this.msg_scroll.scroll_to_reveal_item(index);
                        }
                    }
                    Err(err) => this.messages.set_error(err),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn queue_draft_save(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.selected.clone() else {
            return;
        };
        let body = self.composer_input.read(cx).value().to_string();
        let staged_attachments = self
            .staged_attachments
            .get(&chat)
            .map(|attachment| vec![attachment.transfer.as_str().to_string()])
            .unwrap_or_default();
        let generation = {
            let generation = self
                .draft_generations
                .entry(chat.clone())
                .and_modify(|generation| *generation = generation.saturating_add(1))
                .or_insert(1);
            *generation
        };
        if let Some(summary) = self
            .chats
            .chats
            .iter_mut()
            .find(|summary| summary.id.as_str() == chat)
        {
            summary.draft_preview = if !body.trim().is_empty() {
                Some(body.clone())
            } else {
                self.staged_attachments
                    .get(&chat)
                    .map(|attachment| format!("Attachment: {}", attachment.display_name))
            };
        }
        self.refresh_visible();
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            cx.background_executor().timer(DRAFT_DEBOUNCE).await;
            let current = this
                .update(cx, |this, _| {
                    this.draft_generations.get(&chat).copied() == Some(generation)
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let draft = (!body.trim().is_empty() || !staged_attachments.is_empty()).then(|| {
                wasabi_domain::Draft {
                    body,
                    staged_attachments,
                    ..wasabi_domain::Draft::default()
                }
            });
            if let Err(error) = bridge
                .save_draft(wasabi_domain::ChatId::new(chat), draft)
                .await
            {
                tracing::warn!(error = %error, "draft save failed");
            }
        });
        cx.notify();
    }

    fn queue_outbound_typing(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.selected.clone() else {
            return;
        };
        if !self.session.can_send() {
            return;
        }
        let empty = self.composer_input.read(cx).value().trim().is_empty();
        let generation = {
            let generation = self
                .outbound_typing_generations
                .entry(chat.clone())
                .and_modify(|value| *value = value.saturating_add(1))
                .or_insert(1);
            *generation
        };
        if empty {
            self.stop_outbound_typing(chat, cx);
            return;
        }

        let now = std::time::Instant::now();
        let should_refresh =
            typing_refresh_due(self.outbound_typing_sent_at.get(&chat).copied(), now);
        if should_refresh {
            self.outbound_typing_sent_at.insert(chat.clone(), now);
            self.dispatch_typing(chat.clone(), true, cx);
        }

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            cx.background_executor().timer(TYPING_PAUSE_AFTER).await;
            let should_pause = this
                .update(cx, |this, _| {
                    this.outbound_typing_generations.get(&chat).copied() == Some(generation)
                        && this.outbound_typing_sent_at.remove(&chat).is_some()
                })
                .unwrap_or(false);
            if should_pause
                && let Err(error) = bridge
                    .set_typing(wasabi_domain::ChatId::new(chat), false)
                    .await
            {
                tracing::debug!(kind = %error.kind, "typing pause update failed");
            }
        });
    }

    fn stop_outbound_typing(&mut self, chat: String, cx: &mut Context<Self>) {
        self.outbound_typing_generations
            .entry(chat.clone())
            .and_modify(|value| *value = value.saturating_add(1))
            .or_insert(1);
        if self.outbound_typing_sent_at.remove(&chat).is_some() {
            self.dispatch_typing(chat, false, cx);
        }
    }

    fn dispatch_typing(&self, chat: String, composing: bool, cx: &mut Context<Self>) {
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |_this, _cx| {
            if let Err(error) = bridge
                .set_typing(wasabi_domain::ChatId::new(chat), composing)
                .await
            {
                // Presence is ephemeral and best effort; never surface a
                // composer failure or log protocol detail for it.
                tracing::debug!(kind = %error.kind, "typing state update failed");
            }
        });
    }

    fn spawn_typing_expiry(
        &mut self,
        chat: String,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        spawn_main(cx, async move |this, cx| {
            cx.background_executor().timer(INCOMING_TYPING_TTL).await;
            this.update(cx, |this, cx| {
                if this
                    .typing
                    .get(&chat)
                    .is_some_and(|entry| entry.generation == generation)
                {
                    this.typing.remove(&chat);
                    cx.notify();
                }
            })
            .ok();
        });
    }

    pub(crate) fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.chats.selected.clone() else {
            return;
        };
        let text = self.composer_input.read(cx).value().trim().to_string();
        let attachment = self.staged_attachments.get(&chat_id).cloned();
        if text.is_empty() && attachment.is_none() {
            return;
        }
        if attachment.is_some() && !self.attachment_sending.insert(chat_id.clone()) {
            return;
        }
        self.send_error = None;
        let request = attachment.as_ref().map_or_else(
            || {
                // Text has no staged retry surface, so clear immediately; the
                // durable row arrives via invalidation.
                self.composer_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                wasabi_domain::SendRequest::text(
                    wasabi_domain::ChatId::new(chat_id.clone()),
                    text.clone(),
                )
            },
            |attachment| {
                let caption = (attachment.kind != wasabi_domain::AttachmentKind::Audio)
                    .then(|| text.clone());
                wasabi_domain::SendRequest::attachment(
                    wasabi_domain::ChatId::new(chat_id.clone()),
                    attachment.transfer.clone(),
                    caption,
                )
            },
        );
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let mut result = bridge.send(request).await;
            if result.is_ok()
                && attachment
                    .as_ref()
                    .is_some_and(|attachment| attachment.kind == wasabi_domain::AttachmentKind::Audio)
                && !text.is_empty()
            {
                result = bridge
                    .send(wasabi_domain::SendRequest::text(
                        wasabi_domain::ChatId::new(chat_id.clone()),
                        text.clone(),
                    ))
                    .await;
            }
            let accepted = result.is_ok();
            let transfer = attachment.as_ref().map(|attachment| attachment.transfer.clone());
            let update = this.update(cx, |this, cx| {
                this.attachment_sending.remove(&chat_id);
                if accepted
                    && transfer.as_ref().is_some_and(|transfer| {
                        this.staged_attachments
                            .get(&chat_id)
                            .is_some_and(|current| current.transfer == *transfer)
                    })
                {
                    this.staged_attachments.remove(&chat_id);
                }
                if let Err(error) = &result {
                    tracing::warn!(kind = %error.kind, "send failed");
                    this.send_error = Some(error.ui_message().to_string());
                }
                cx.notify();
                (this.window_handle, this.composer_input.clone())
            });
            if accepted
                && let Ok((window_handle, input)) = update
            {
                window_handle
                    .update(cx, |_, window, cx| {
                        input.update(cx, |state, cx| {
                            if state.value().trim() == text {
                                state.set_value("", window, cx);
                            }
                        });
                    })
                    .ok();
            }
        });
        cx.notify();
    }

    pub(crate) fn choose_attachment(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.selected.clone() else {
            return;
        };
        if self.staged_attachments.contains_key(&chat)
            || !self.attachment_staging.insert(chat.clone())
        {
            return;
        }
        self.send_error = None;
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Attach".into()),
        });
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let selected = paths.await.ok().and_then(Result::ok).flatten();
            let result = match selected.and_then(|mut paths| paths.pop()) {
                Some(path) => bridge
                    .stage_attachment(wasabi_domain::ChatId::new(chat.clone()), path)
                    .await
                    .map(Some),
                None => Ok(None),
            };
            this.update(cx, |this, cx| {
                this.attachment_staging.remove(&chat);
                match result {
                    Ok(Some(attachment)) => {
                        this.staged_attachments.insert(chat.clone(), attachment);
                        if this.chats.selected.as_deref() == Some(chat.as_str()) {
                            this.queue_draft_save(cx);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "attachment staging failed");
                        this.send_error = Some(error.ui_message().to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn remove_attachment(&mut self, chat: String, cx: &mut Context<Self>) {
        let Some(attachment) = self.staged_attachments.remove(&chat) else {
            return;
        };
        self.attachment_sending.remove(&chat);
        self.queue_draft_save(cx);
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |_this, _cx| {
            if let Err(error) = bridge.cancel_transfer(attachment.transfer).await {
                tracing::warn!(kind = %error.kind, "attachment cleanup failed");
            }
        });
        cx.notify();
    }

    pub(crate) fn download_media(
        &mut self,
        chat: wasabi_domain::ChatId,
        media: wasabi_domain::MediaId,
        cx: &mut Context<Self>,
    ) {
        let key = (chat.clone(), media.clone());
        if matches!(
            self.media_downloads.get(&key),
            Some(MediaDownloadUi::Downloading | MediaDownloadUi::Ready(_))
        ) {
            return;
        }
        self.media_downloads
            .insert(key.clone(), MediaDownloadUi::Downloading);
        let request = wasabi_domain::MediaDownloadRequest { chat, media };
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.download_media(request).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(cached) => {
                        debug_assert_eq!(cached.media, key.1);
                        this.media_downloads
                            .insert(key, MediaDownloadUi::Ready(cached.path));
                    }
                    Err(error) => {
                        // Do not log `detail`: transport failures may contain a
                        // private CDN URL. The typed kind is sufficient here.
                        tracing::warn!(kind = %error.kind, "media download failed");
                        this.media_downloads.insert(key, MediaDownloadUi::Failed);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn request_pairing(&mut self, cx: &mut Context<Self>) {
        self.start_pairing_request(cx, false);
    }

    pub(crate) fn show_phone_pairing(&mut self, cx: &mut Context<Self>) {
        self.session.use_phone_pairing = true;
        self.session.phone_pair_error = None;
        cx.notify();
    }

    pub(crate) fn show_qr_pairing(&mut self, cx: &mut Context<Self>) {
        self.phone_pair_request_gen.fetch_add(1, Ordering::AcqRel);
        self.phone_pair_ticker_gen.fetch_add(1, Ordering::AcqRel);
        self.session.use_phone_pairing = false;
        self.session.phone_pair_code = None;
        self.session.phone_pair_deadline = None;
        self.session.phone_pair_requesting = false;
        self.session.phone_pair_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |_this, _cx| {
            let _ = bridge.cancel_phone_pairing().await;
        });
        cx.notify();
    }

    pub(crate) fn request_phone_pairing(&mut self, cx: &mut Context<Self>) {
        if self.session.phone_pair_requesting || !self.bridge.commands_accepted() {
            return;
        }
        let input = self.phone_pair_input.read(cx).value().to_string();
        let phone = match wasabi_domain::PairingPhoneNumber::parse(&input) {
            Ok(phone) => phone,
            Err(message) => {
                self.session.phone_pair_error = Some(message.to_string());
                cx.notify();
                return;
            }
        };
        let request_generation = self
            .phone_pair_request_gen
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        self.phone_pair_ticker_gen.fetch_add(1, Ordering::AcqRel);
        self.session.phone_pair_code = None;
        self.session.phone_pair_deadline = None;
        self.session.phone_pair_requesting = true;
        self.session.phone_pair_error = None;
        cx.notify();

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.start_phone_pairing(phone).await;
            this.update(cx, |this, cx| {
                if this.phone_pair_request_gen.load(Ordering::Acquire) != request_generation {
                    return;
                }
                this.session.phone_pair_requesting = false;
                match result {
                    Ok(pairing) => {
                        this.session.phone_pair_code = Some(pairing.code);
                        this.session.phone_pair_deadline =
                            Some(std::time::Instant::now() + pairing.expires_in);
                        this.spawn_phone_pair_countdown(cx);
                    }
                    Err(error) => {
                        this.session.phone_pair_error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    fn restart_pairing(&mut self, cx: &mut Context<Self>) {
        self.start_pairing_request(cx, true);
    }

    fn start_pairing_request(&mut self, cx: &mut Context<Self>, restart: bool) {
        if self.session.pairing_requesting || !self.bridge.commands_accepted() {
            return;
        }

        let request_generation = self.pairing_request_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.qr_ticker_gen.fetch_add(1, Ordering::AcqRel);
        self.session.qr_code = None;
        self.session.qr_deadline = None;
        self.session.pairing_requesting = true;
        self.session.pairing_error = None;
        cx.notify();

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = if restart {
                match bridge.stop_session().await {
                    Ok(()) => bridge.start_pairing().await,
                    Err(err) => Err(err),
                }
            } else {
                bridge.start_pairing().await
            };
            this.update(cx, |this, cx| {
                if this.pairing_request_gen.load(Ordering::Acquire) != request_generation {
                    return;
                }
                this.session.pairing_requesting = false;
                if let Err(err) = result
                    && this.session.qr_code.is_none()
                    && !matches!(
                        this.session.state,
                        wasabi_core::state::SessionState::Connecting
                            | wasabi_core::state::SessionState::Connected
                    )
                {
                    tracing::warn!(error = %err, "pairing request failed");
                    this.session.pairing_error = Some(err);
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(crate) fn toggle_right_panel(&mut self, cx: &mut Context<Self>) {
        if self.show_right_panel {
            self.close_right_panel(cx);
            return;
        }
        self.show_right_panel = true;
        self.load_conversation_details(cx);
    }

    pub(crate) fn close_right_panel(&mut self, cx: &mut Context<Self>) {
        self.details_gen.fetch_add(1, Ordering::AcqRel);
        self.show_right_panel = false;
        self.details_loading = false;
        cx.notify();
    }

    fn load_conversation_details(&mut self, cx: &mut Context<Self>) {
        let Some((chat, kind)) = self
            .chats
            .selected
            .as_ref()
            .and_then(|selected| {
                self.chats
                    .chats
                    .iter()
                    .find(|summary| summary.id.as_str() == selected)
                    .map(|summary| (selected.clone(), summary.kind))
            })
        else {
            self.details_error = Some("Conversation information is unavailable".to_string());
            cx.notify();
            return;
        };
        let generation = self.details_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.conversation_details = None;
        self.details_loading = true;
        self.details_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = match kind {
                ChatKind::Group => bridge
                    .group_details(chat)
                    .await
                    .map(ConversationDetails::Group),
                ChatKind::Direct => bridge
                    .direct_contact_details(chat)
                    .await
                    .map(ConversationDetails::Direct),
                ChatKind::Newsletter | ChatKind::System => {
                    Err("Information is not available for this conversation type".to_string())
                }
            };
            this.update(cx, |this, cx| {
                if this.details_gen.load(Ordering::Acquire) != generation
                    || !this.show_right_panel
                {
                    return;
                }
                this.details_loading = false;
                match result {
                    Ok(details) => this.conversation_details = Some(details),
                    Err(error) => this.details_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn toggle_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(selected) = self.chats.selected.clone() else {
            return;
        };
        let Some(index) = self
            .chats
            .chats
            .iter()
            .position(|chat| chat.id.as_str() == selected)
        else {
            return;
        };
        let favorite = !self.chats.chats[index].favorite;
        self.chats.chats[index].favorite = favorite;
        self.refresh_visible();
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge
                .set_favorite(wasabi_domain::ChatId::new(selected.clone()), favorite)
                .await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    if let Some(chat) = this
                        .chats
                        .chats
                        .iter_mut()
                        .find(|chat| chat.id.as_str() == selected)
                        && chat.favorite == favorite
                    {
                        chat.favorite = !favorite;
                    }
                    this.details_error = Some(error);
                    this.refresh_visible();
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn perform_message_action(
        &mut self,
        action: wasabi_domain::MessageAction,
        cx: &mut Context<Self>,
    ) {
        let target = action.target().clone();
        let desired_star = match &action {
            wasabi_domain::MessageAction::Star { starred, .. } => Some(*starred),
            _ => None,
        };
        if let Some(starred) = desired_star
            && let Some(row) = self.messages.rows.iter_mut().find(|row| {
                row.chat == target.chat && row.id == target.message
            })
        {
            row.starred = starred;
        }
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.perform_message_action(action).await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    if let Some(starred) = desired_star
                        && let Some(row) = this.messages.rows.iter_mut().find(|row| {
                            row.chat == target.chat && row.id == target.message
                        })
                        && row.starred == starred
                    {
                        row.starred = !starred;
                    }
                    this.send_error = Some(error.ui_message().to_string());
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn open_message_actions(
        &mut self,
        message: wasabi_domain::MessageId,
        cx: &mut Context<Self>,
    ) {
        self.message_overlay = Some(MessageOverlay::Actions(message));
        cx.notify();
    }

    pub(crate) fn close_message_overlay(&mut self, cx: &mut Context<Self>) {
        self.message_overlay = None;
        cx.notify();
    }

    pub(crate) fn confirm_message_action(
        &mut self,
        action: wasabi_domain::MessageAction,
        cx: &mut Context<Self>,
    ) {
        self.message_overlay = Some(MessageOverlay::Confirm(action));
        cx.notify();
    }

    pub(crate) fn run_confirmed_message_action(&mut self, cx: &mut Context<Self>) {
        let Some(MessageOverlay::Confirm(action)) = self.message_overlay.take() else {
            return;
        };
        self.perform_message_action(action, cx);
    }

    pub(crate) fn copy_message(
        &mut self,
        message: wasabi_domain::MessageId,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.messages.rows.iter().find(|row| row.id == message) else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            crate::state::messages::body_text(row),
        ));
        self.message_overlay = None;
        cx.notify();
    }

    pub(crate) fn react_to_message(
        &mut self,
        message: wasabi_domain::MessageId,
        emoji: String,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.messages.rows.iter().find(|row| row.id == message) else {
            return;
        };
        let action = wasabi_domain::MessageAction::React {
            target: row.into(),
            emoji,
        };
        self.message_overlay = None;
        self.perform_message_action(action, cx);
    }

    pub(crate) fn dismiss_overlay_or_drawer(&mut self, cx: &mut Context<Self>) {
        if self.settings_overlay.take().is_some() || self.message_overlay.take().is_some() {
            cx.notify();
        } else {
            self.close_right_panel(cx);
        }
    }

    fn consider_notification(&mut self, chat: String, cx: &mut Context<Self>) {
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let candidate = bridge.notification_candidate(chat).await;
            this.update(cx, |this, _cx| {
                let Ok(Some(candidate)) = candidate else {
                    return;
                };
                let identity = (
                    candidate.chat.as_str().to_string(),
                    candidate.message.as_str().to_string(),
                );
                if !this.remember_notification(identity) {
                    return;
                }
                if !crate::notifications::should_deliver(
                    &candidate,
                    &this.settings,
                    this.window_active,
                    this.notification_started_at_ms,
                ) {
                    return;
                }
                this.notifications.show(candidate, &this.settings);
            })
            .ok();
        });
    }

    fn remember_notification(&mut self, identity: (String, String)) -> bool {
        if !self.notification_seen.insert(identity.clone()) {
            return false;
        }
        self.notification_seen_order.push_back(identity);
        while self.notification_seen_order.len() > NOTIFICATION_DEDUPE_LIMIT {
            if let Some(expired) = self.notification_seen_order.pop_front() {
                self.notification_seen.remove(&expired);
            }
        }
        true
    }

    pub(crate) fn perform_chat_action(
        &mut self,
        action: wasabi_domain::ChatAction,
        cx: &mut Context<Self>,
    ) {
        let chat_id = action.chat().as_str().to_string();
        let Some(index) = self
            .chats
            .chats
            .iter()
            .position(|chat| chat.id.as_str() == chat_id)
        else {
            return;
        };
        let previous = {
            let chat = &self.chats.chats[index];
            (
                chat.pinned_at_ms,
                chat.muted_until_ms,
                chat.archived,
                chat.unread_count,
            )
        };
        {
            let chat = &mut self.chats.chats[index];
            match &action {
                wasabi_domain::ChatAction::Pin { pinned, .. } => {
                    chat.pinned_at_ms = pinned.then(|| chrono::Utc::now().timestamp_millis());
                }
                wasabi_domain::ChatAction::Mute { muted, .. } => {
                    chat.muted_until_ms = muted.then_some(i64::MAX);
                }
                wasabi_domain::ChatAction::Archive { archived, .. } => {
                    chat.archived = *archived;
                }
                wasabi_domain::ChatAction::MarkRead { read, .. } => {
                    chat.unread_count = if *read { 0 } else { -1 };
                }
            }
        }
        self.refresh_visible();
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.perform_chat_action(action).await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    if let Some(chat) = this
                        .chats
                        .chats
                        .iter_mut()
                        .find(|chat| chat.id.as_str() == chat_id)
                    {
                        chat.pinned_at_ms = previous.0;
                        chat.muted_until_ms = previous.1;
                        chat.archived = previous.2;
                        chat.unread_count = previous.3;
                    }
                    this.send_error = Some(error.ui_message().to_string());
                    this.refresh_visible();
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn select_settings_section(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        self.settings_feedback = None;
        if section == SettingsSection::Storage {
            self.refresh_media_cache_usage(cx);
        }
        cx.notify();
    }

    pub(crate) fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.save() {
            self.settings_feedback = Some(SettingsFeedback::Error(format!(
                "Could not save settings: {error}"
            )));
        }
        cx.notify();
    }

    pub(crate) fn choose_download_directory(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose download folder".into()),
        });
        spawn_main(cx, async move |this, cx| {
            let selected = paths.await.ok().and_then(Result::ok).flatten();
            let Some(path) = selected.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            this.update(cx, |this, cx| {
                this.settings.download_path = path.to_string_lossy().into_owned();
                this.settings_feedback = Some(SettingsFeedback::Success(
                    "Download folder updated".to_string(),
                ));
                this.save_settings(cx);
            })
            .ok();
        });
    }

    pub(crate) fn refresh_media_cache_usage(&mut self, cx: &mut Context<Self>) {
        if self.media_cache_loading {
            return;
        }
        self.media_cache_loading = true;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.media_cache_usage().await;
            this.update(cx, |this, cx| {
                this.media_cache_loading = false;
                match result {
                    Ok(bytes) => this.media_cache_usage_bytes = Some(bytes),
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "media cache usage failed");
                        this.settings_feedback = Some(SettingsFeedback::Error(
                            error.ui_message().to_string(),
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn set_media_cache_quota(&mut self, quota_mb: u64, cx: &mut Context<Self>) {
        if self.media_cache_loading
            || !crate::state::settings::CACHE_QUOTA_CHOICES_MB.contains(&quota_mb)
        {
            return;
        }
        self.media_cache_loading = true;
        self.settings_feedback = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge
                .set_media_cache_quota(quota_mb.saturating_mul(1024 * 1024))
                .await;
            this.update(cx, |this, cx| {
                this.media_cache_loading = false;
                match result {
                    Ok(bytes) => {
                        this.settings.cache_quota_mb = quota_mb;
                        this.media_cache_usage_bytes = Some(bytes);
                        this.settings_feedback = Some(SettingsFeedback::Success(format!(
                            "Cache quota set to {quota_mb} MB"
                        )));
                        this.save_settings(cx);
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "media cache quota failed");
                        this.settings_feedback = Some(SettingsFeedback::Error(
                            error.ui_message().to_string(),
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn confirm_clear_media_cache(&mut self, cx: &mut Context<Self>) {
        self.settings_overlay = Some(SettingsOverlay::ClearMediaCache);
        cx.notify();
    }

    pub(crate) fn confirm_logout(&mut self, cx: &mut Context<Self>) {
        if !self.logout_in_progress {
            self.settings_overlay = Some(SettingsOverlay::Logout);
            cx.notify();
        }
    }

    pub(crate) fn close_settings_overlay(&mut self, cx: &mut Context<Self>) {
        self.settings_overlay = None;
        cx.notify();
    }

    pub(crate) fn run_clear_media_cache(&mut self, cx: &mut Context<Self>) {
        if self.media_cache_loading {
            return;
        }
        self.settings_overlay = None;
        self.media_cache_loading = true;
        self.settings_feedback = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.clear_media_cache().await;
            this.update(cx, |this, cx| {
                this.media_cache_loading = false;
                match result {
                    Ok(()) => {
                        this.media_cache_usage_bytes = Some(0);
                        this.settings_feedback = Some(SettingsFeedback::Success(
                            "Media cache cleared".to_string(),
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "media cache clear failed");
                        this.settings_feedback = Some(SettingsFeedback::Error(
                            error.ui_message().to_string(),
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn run_confirmed_settings_action(&mut self, cx: &mut Context<Self>) {
        match self.settings_overlay.take() {
            Some(SettingsOverlay::ClearMediaCache) => self.run_clear_media_cache(cx),
            Some(SettingsOverlay::Logout) => self.run_logout(cx),
            None => {}
        }
    }

    fn run_logout(&mut self, cx: &mut Context<Self>) {
        if self.logout_in_progress {
            return;
        }
        self.logout_in_progress = true;
        self.settings_feedback = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.logout().await;
            this.update(cx, |this, cx| {
                this.logout_in_progress = false;
                match result {
                    Ok(()) => {
                        this.session = SessionMirror::new();
                        this.nav_destination = NavDestination::Chats;
                        this.show_right_panel = false;
                        this.message_overlay = None;
                        this.settings_overlay = None;
                        this.typing.clear();
                        this.notification_seen.clear();
                        this.notification_seen_order.clear();
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "account logout failed");
                        this.settings_feedback = Some(SettingsFeedback::Error(
                            error.ui_message().to_string(),
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn refresh_visible(&mut self) {
        self.chats.visible_cache = self.chats.visible();
    }

    fn sync_message_list(&mut self, before: Vec<crate::state::messages::TimelineKey>) {
        let after = self.messages.timeline_keys();
        if self.msg_scroll.item_count() != before.len() {
            self.msg_scroll.reset(after.len());
            return;
        }

        let offset = self.msg_scroll.logical_scroll_top();
        let anchor = before.get(offset.item_ix).cloned();
        let (old_range, replacement_count) = timeline_splice(&before, &after);
        if !old_range.is_empty() || replacement_count > 0 {
            self.msg_scroll.splice(old_range, replacement_count);
        }

        if let Some(anchor) = anchor
            && let Some(item_ix) = after.iter().position(|item| item == &anchor)
        {
            self.msg_scroll.scroll_to(gpui::ListOffset {
                item_ix,
                offset_in_item: offset.offset_in_item,
            });
        }
    }

    // ---- History paging ----------------------------------------------------

    /// Called from the render path when the visible range approaches the top
    /// of the window; concurrency-guarded by `loading_older`.
    pub(crate) fn load_older_history(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.messages.chat_id.clone() else {
            return;
        };
        let Some(cursor) = self.messages.older_cursor() else {
            return;
        };
        self.messages.loading_older = true;
        let generation = self.current_messages_gen();

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let page = bridge
                .load_message_page(&chat, Some(cursor), MESSAGE_PAGE_LIMIT)
                .await;
            this.update(cx, |this, cx| {
                if this.messages_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match page {
                    Ok(page) => {
                        let before = this.messages.timeline_keys();
                        this.messages.prepend_older(&page);
                        this.sync_message_list(before);
                    }
                    Err(err) => this.messages.set_error(err),
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(crate) fn jump_to_newest_messages(&mut self, cx: &mut Context<Self>) {
        if self.pending_new_messages == 0 && !self.messages.has_more_newer {
            self.msg_scroll.scroll_to_end();
            return;
        }
        self.pending_new_messages = 0;
        self.near_bottom = true;
        self.refresh_current_messages(cx);
    }

    /// Load the next bounded page toward the newest end of an anchored
    /// search result without replacing or jumping the window being read.
    pub(crate) fn load_newer_history(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.messages.chat_id.clone() else {
            return;
        };
        let Some(anchor) = self.messages.newer_anchor() else {
            return;
        };
        self.messages.loading_newer = true;
        let generation = self.current_messages_gen();
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let context = bridge
                .load_message_context(chat, anchor, 0, MESSAGE_PAGE_LIMIT)
                .await;
            this.update(cx, |this, cx| {
                if this.messages_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match context {
                    Ok(context) => {
                        let before = this.messages.timeline_keys();
                        this.messages.append_newer_context(&context);
                        this.sync_message_list(before);
                    }
                    Err(error) => {
                        this.messages.loading_newer = false;
                        this.send_error = Some(error);
                    }
                }
                cx.notify();
            })
            .ok();
        });
    }

    // ---- Refresh paths (invalidations) -------------------------------------

    pub(crate) fn refresh_chats(&mut self, cx: &mut Context<Self>) {
        let generation = self.next_chats_gen();
        self.chats.loading = true;
        let scope = self.chats.scope;

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let page = bridge.load_chat_page(scope, None, CHAT_PAGE_LIMIT).await;
            this.update(cx, |this, cx| {
                if this.chats_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match page {
                    Ok(rows) => {
                        this.chats.set_page(rows);
                        this.refresh_visible();
                    }
                    Err(err) => this.chats.set_error(err),
                }
                cx.notify();
            })
            .ok();
        });
    }

    pub(crate) fn load_more_chats(&mut self, cx: &mut Context<Self>) {
        if self.chats.loading_more {
            return;
        }
        let Some(after) = self.chats.next_cursor() else {
            return;
        };
        self.chats.loading_more = true;
        let generation = self.current_chats_gen();
        let scope = self.chats.scope;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let page = bridge
                .load_chat_page(scope, Some(after), CHAT_PAGE_LIMIT)
                .await;
            this.update(cx, |this, cx| {
                if this.chats_gen.load(Ordering::Acquire) != generation
                    || this.chats.scope != scope
                {
                    return;
                }
                match page {
                    Ok(page) => {
                        this.chats.append_page(page);
                        this.refresh_visible();
                    }
                    Err(error) => this.chats.set_error(error),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn refresh_current_messages(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.messages.chat_id.clone() else {
            return;
        };
        // Cancel anything stale, then take this load's generation.
        self.messages_gen.fetch_add(1, Ordering::AcqRel);
        let generation = self.next_messages_gen();

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let page = bridge
                .load_message_page(&chat, None, MESSAGE_PAGE_LIMIT * 3)
                .await;
            this.update(cx, |this, cx| {
                if this.messages_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match &page {
                    Ok(page) => {
                        let before = this.messages.timeline_keys();
                        if this.near_bottom || this.messages.rows.is_empty() {
                            this.messages.anchor_newest(page);
                            this.sync_message_list(before);
                            this.msg_scroll.scroll_to_end();
                            this.pending_new_messages = 0;
                        } else {
                            // Mid-history: fold newer rows in place.
                            let unseen = this.messages.merge_newer(page);
                            this.sync_message_list(before);
                            this.pending_new_messages =
                                this.pending_new_messages.max(unseen);
                        }
                    }
                    Err(err) => this.messages.set_error(err.clone()),
                }
                cx.notify();
            })
            .ok();
        });
    }

    // ---- Background loops --------------------------------------------------

    fn spawn_hydration(&mut self, cx: &mut Context<Self>) {
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let recovered = bridge.recover_staged_attachments().await;
            let page = bridge
                .load_chat_page(ChatScope::Active, None, CHAT_PAGE_LIMIT)
                .await;
            let first_chat = page
                .as_ref()
                .ok()
                .and_then(|page| page.rows.first().map(|c| c.id.as_str().to_string()));

            this.update(cx, |this, cx| {
                match &page {
                    Ok(page) => {
                        this.chats.set_page(page.clone());
                        this.refresh_visible();
                    }
                    Err(err) => this.chats.set_error(err.clone()),
                }
                if let Ok(recovered) = &recovered {
                    for (chat, attachment) in recovered {
                        this.staged_attachments
                            .insert(chat.as_str().to_string(), attachment.clone());
                    }
                }
                cx.notify();
            })
            .ok();

            if let Some(chat) = first_chat {
                this.update_in(cx, |this, window, cx| {
                    this.select_chat(chat, window, cx)
                })
                .ok();
            }

            // Startup connect: paired accounts come up directly; unpaired
            // ones surface QR events through the watches.
            if let Err(err) = bridge.connect_session().await {
                tracing::info!(error = %err, "startup connect deferred");
            }
        });
    }

    fn spawn_invalidation_loop(&mut self, cx: &mut Context<Self>) {
        let mut feed = self.bridge.invalidations().subscribe();
        spawn_main(cx, async move |this, cx| {
            loop {
                let Some(invalidation) = feed.recv().await else {
                    break;
                };
                let Some(handle) = this.upgrade() else {
                    break;
                };
                handle.update(cx, |this, cx| {
                    use wasabi_core::events::Invalidation;
                    match invalidation {
                        Invalidation::Chats | Invalidation::Contacts => this.refresh_chats(cx),
                        Invalidation::Messages { chat } => {
                            if this.chats.selected.as_deref() == Some(chat.as_str()) {
                                this.refresh_current_messages(cx);
                            }
                            this.consider_notification(chat, cx);
                        }
                    }
                });
            }
        });
    }

    fn spawn_state_watch(&mut self, cx: &mut Context<Self>) {
        let Some(mut rx) = self.bridge.subscribe_state() else {
            return;
        };
        spawn_main(cx, async move |this, cx| {
            // Prime with the current value before waiting for changes.
            let initial = rx.borrow_and_update().clone();
            apply_state(this.clone(), cx, initial).ok();
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let state = rx.borrow_and_update().clone();
                if apply_state(this.clone(), cx, state).is_err() {
                    break;
                }
            }
        });
    }

    fn spawn_qr_watch(&mut self, cx: &mut Context<Self>) {
        let Some(mut rx) = self.bridge.subscribe_qr() else {
            return;
        };
        spawn_main(cx, async move |this, cx| {
            let initial = rx.borrow_and_update().clone();
            apply_qr(this.clone(), cx, initial).ok();
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let qr = rx.borrow_and_update().clone();
                if apply_qr(this.clone(), cx, qr).is_err() {
                    break;
                }
            }
        });
    }

    fn spawn_typing_watch(&mut self, cx: &mut Context<Self>) {
        let Some(mut rx) = self.bridge.subscribe_typing() else {
            return;
        };
        spawn_main(cx, async move |this, cx| {
            loop {
                match rx.recv().await {
                    Ok(update) => {
                        let Some(entity) = this.upgrade() else {
                            break;
                        };
                        entity.update(cx, |this, cx| {
                            let chat = update.chat.as_str().to_string();
                            if update.state == wasabi_domain::TypingState::Paused {
                                this.typing.remove(&chat);
                            } else {
                                let generation = this
                                    .typing
                                    .get(&chat)
                                    .map_or(1, |entry| entry.generation.saturating_add(1));
                                this.typing.insert(
                                    chat.clone(),
                                    TypingDisplay {
                                        state: update.state,
                                        participant: update.participant,
                                        generation,
                                    },
                                );
                                this.spawn_typing_expiry(chat, generation, cx);
                            }
                            cx.notify();
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(entity) = this.upgrade() {
                            entity.update(cx, |this, cx| {
                                // Ephemeral state cannot be reconstructed from
                                // a lagged feed; clearing is the honest state.
                                this.typing.clear();
                                cx.notify();
                            });
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn spawn_notification_click_watch(
        &mut self,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<wasabi_domain::NotificationCandidate>,
        cx: &mut Context<Self>,
    ) {
        let window_handle = self.window_handle;
        spawn_main(cx, async move |this, cx| {
            while let Some(candidate) = receiver.recv().await {
                let Some(entity) = this.upgrade() else {
                    break;
                };
                cx.update_window(window_handle, |_, window, cx| {
                    window.activate_window();
                    entity.update(cx, |this, cx| {
                        let chat = candidate.chat.as_str().to_string();
                        if !this
                            .chats
                            .chats
                            .iter()
                            .any(|summary| summary.id == candidate.chat)
                        {
                            this.chats.chats.push(notification_chat_summary(&candidate));
                            this.refresh_visible();
                        }
                        this.select_nav(NavDestination::Chats, cx);
                        this.select_chat(chat, window, cx);
                    })
                })
                .ok();
            }
        });
    }

    fn spawn_countdown_ticker(&mut self, cx: &mut Context<Self>) {
        let generation = self.qr_ticker_gen.fetch_add(1, Ordering::AcqRel) + 1;
        spawn_main(cx, async move |this, cx| {
            loop {
                cx.background_executor().timer(COUNTDOWN_TICK).await;
                let alive = this
                    .update(cx, |this, cx| {
                        let superseded = this.qr_ticker_gen.load(Ordering::Acquire) != generation;
                        if superseded {
                            cx.notify();
                            return false;
                        }

                        let expired = this
                            .session
                            .qr_deadline
                            .is_some_and(|deadline| deadline <= std::time::Instant::now());
                        if expired {
                            this.session.qr_code = None;
                            this.session.qr_deadline = None;
                            this.qr_ticker_gen.fetch_add(1, Ordering::AcqRel);
                            if matches!(
                                this.session.state,
                                wasabi_core::state::SessionState::Pairing
                            ) && !this.session.use_phone_pairing
                            {
                                this.restart_pairing(cx);
                            }
                            cx.notify();
                            return false;
                        }

                        let alive = this.session.qr_deadline.is_some();
                        cx.notify();
                        alive
                    })
                    .unwrap_or(false);
                cx.refresh();
                if !alive {
                    break;
                }
            }
        });
    }

    fn spawn_phone_pair_countdown(&mut self, cx: &mut Context<Self>) {
        let generation = self
            .phone_pair_ticker_gen
            .fetch_add(1, Ordering::AcqRel)
            + 1;
        spawn_main(cx, async move |this, cx| {
            loop {
                cx.background_executor().timer(COUNTDOWN_TICK).await;
                let alive = this
                    .update(cx, |this, cx| {
                        if this.phone_pair_ticker_gen.load(Ordering::Acquire) != generation {
                            return false;
                        }
                        let expired = this
                            .session
                            .phone_pair_deadline
                            .is_some_and(|deadline| deadline <= std::time::Instant::now());
                        if expired {
                            this.session.phone_pair_code = None;
                            this.session.phone_pair_deadline = None;
                            this.session.phone_pair_error =
                                Some("This code expired. Request a new one.".to_string());
                            this.phone_pair_ticker_gen.fetch_add(1, Ordering::AcqRel);
                            let bridge = Arc::clone(&this.bridge);
                            spawn_main(cx, async move |_this, _cx| {
                                let _ = bridge.cancel_phone_pairing().await;
                            });
                            cx.notify();
                            return false;
                        }
                        let alive = this.session.phone_pair_deadline.is_some();
                        cx.notify();
                        alive
                    })
                    .unwrap_or(false);
                cx.refresh();
                if !alive {
                    break;
                }
            }
        });
    }

    // ---- Generation helpers ------------------------------------------------

    fn next_chats_gen(&self) -> u64 {
        self.chats_gen.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn current_chats_gen(&self) -> u64 {
        self.chats_gen.load(Ordering::Acquire)
    }

    fn next_messages_gen(&self) -> u64 {
        self.messages_gen.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn current_messages_gen(&self) -> u64 {
        self.messages_gen.load(Ordering::Acquire)
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.window_active = window.is_window_active();
        // Keep the filtered projection in sync before any list reads it.
        self.refresh_visible();

        let store_ready = self.bridge.store_ready();
        let pairing_active = matches!(
            self.session.state,
            wasabi_core::state::SessionState::Pairing
        ) || self.session.qr_deadline.is_some()
            || self.session.pairing_requesting
            || self.session.pairing_error.is_some();

        if !store_ready {
            return opening_storage().into_any_element();
        }

        if !self.session.connected_once {
            return pairing_gate(self, cx);
        }

        div()
            .id("root")
            .size_full()
            .flex()
            .bg(theme::canvas())
            .text_color(theme::text_primary())
            .text_size(px(theme::TEXT_SIZE))
            .track_focus(&self.focus)
            .key_context(MAIN_KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                let search = this.search_input.clone();
                search.update(cx, |state, cx| state.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, _, cx| {
                this.select_nav(NavDestination::Settings, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseInfo, _, cx| {
                this.dismiss_overlay_or_drawer(cx);
            }))
            .child(main_content(self, window, cx, pairing_active))
            .into_any_element()
    }
}

fn nav_rail(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let items = [(IconName::Inbox, NavDestination::Chats, "Chats")];

    let mut rail = div()
        .w(px(theme::NAV_W))
        .h_full()
        .flex_shrink(0.0)
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.0))
        .py(px(8.0))
        .bg(theme::nav_rail())
        .border_r_1()
        .border_color(theme::border());

    for (index, (icon, destination, label)) in items.into_iter().enumerate() {
        let active = this.nav_destination == destination;
        let mut item = div()
            .id(("nav-item", index))
            .size(px(40.0))
            .rounded(px(theme::RADIUS_LG))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(18.0))
            .aria_label(label)
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx));
        if active {
            item = item.bg(theme::row_selected()).text_color(theme::accent_text());
        } else {
            item = item
                .text_color(theme::text_secondary())
                .hover(|s| s.bg(theme::row_hover()));
        }
        item = item.on_click(cx.listener(move |this, _, _, cx| {
            this.select_nav(destination, cx);
        }));
        rail = rail.child(item.child(Icon::new(icon).size(px(20.0))));
    }

    let settings_active = this.nav_destination == NavDestination::Settings;
    rail = rail.child(
        div()
            .flex_1()
            .flex()
            .flex_col()
            .justify_end()
            .gap(px(8.0))
            .child(
                div()
                    .id("nav-settings")
                    .size(px(40.0))
                    .rounded(px(theme::RADIUS_LG))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(18.0))
                    .aria_label("Settings")
                    .tooltip(|window, cx| Tooltip::new("Settings").build(window, cx))
                    .when(settings_active, |item| {
                        item.bg(theme::row_selected()).text_color(theme::accent_text())
                    })
                    .when(!settings_active, |item| {
                        item.text_color(theme::text_secondary())
                    })
                    .hover(|s| s.bg(theme::row_hover()))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_nav(NavDestination::Settings, cx);
                    }))
                    .child(Icon::new(IconName::Settings2).size(px(20.0))),
            )
            .child(
                div()
                    .id("nav-account")
                    .size(px(38.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(theme::accent_text())
                    .text_color(theme::text_on_accent())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .aria_label("Account")
                    .tooltip(|window, cx| Tooltip::new("Account").build(window, cx))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_nav(NavDestination::Settings, cx);
                    }))
                    .child("W"),
            ),
    );
    rail
}

fn main_content(
    this: &mut MainWindow,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
    pairing_active: bool,
) -> gpui::AnyElement {
    if this.nav_destination == NavDestination::Settings {
        return div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .child(nav_rail(this, cx))
            .child(settings::settings_page(this, cx))
            .into_any_element();
    }

    div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .child(nav_rail(this, cx))
        .child(chat_pane(this, window, cx))
        .child(center_area(this, true, pairing_active, window, cx))
        .into_any_element()
}

fn pairing_gate(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::AnyElement {
    div()
        .id("pairing-gate")
        .size_full()
        .flex()
        .flex_col()
        .bg(theme::canvas())
        .child(pairing::pairing_panel(this, cx))
        .into_any_element()
}

fn chat_pane(
    this: &mut MainWindow,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    div()
        .w(px(theme::CHAT_LIST_W))
        .h_full()
        .flex_shrink(0.0)
        .flex()
        .flex_col()
        .bg(theme::surface())
        .border_r_1()
        .border_color(theme::border())
        .child(chat_list::pane_header(this, cx))
        .child(chat_list::search_bar(this))
        .child(chat_list::filter_bar(this, cx))
        .child(chat_list::chat_list(this, window, cx))
}

fn center_area(
    this: &mut MainWindow,
    store_ready: bool,
    pairing_active: bool,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) -> gpui::AnyElement {
    if !store_ready {
        return opening_storage().into_any_element();
    }
    if pairing_active || this.session.needs_pairing() {
        return pairing::pairing_panel(this, cx).into_any_element();
    }

    let conversation = conversation::conversation(this, window, cx);
    let narrow_drawer = window.viewport_size().width < px(1180.0);
    let mut row = div().flex_1().min_w(px(0.0)).flex().relative();
    row = row.child(conversation);
    if this.show_right_panel && narrow_drawer {
        row = row.child(
            div()
                .absolute()
                .size_full()
                .flex()
                .justify_end()
                .child(
                    div()
                        .id("drawer-scrim")
                        .absolute()
                        .size_full()
                        .bg(theme::scrim())
                        .on_click(cx.listener(|this, _, _, cx| this.close_right_panel(cx))),
                )
                .child(div().relative().child(right_panel::info_panel(this, cx))),
        );
    } else if this.show_right_panel {
        row = row.child(right_panel::info_panel(this, cx));
    }
    if this.message_overlay.is_some() {
        row = row.child(conversation::message_overlay(this, cx));
    }
    row.into_any_element()
}

/// Hydrate-first degraded state: shown only when storage is not open yet.
fn opening_storage() -> gpui::Div {
    div()
        .size_full()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::text_secondary())
        .child("Opening storage…")
}

fn notification_chat_summary(
    candidate: &wasabi_domain::NotificationCandidate,
) -> wasabi_domain::ChatSummary {
    let raw = candidate.chat.as_str();
    let kind = if raw.ends_with("@g.us") {
        wasabi_domain::ChatKind::Group
    } else if raw.ends_with("@newsletter") {
        wasabi_domain::ChatKind::Newsletter
    } else if raw.ends_with("@broadcast") {
        wasabi_domain::ChatKind::System
    } else {
        wasabi_domain::ChatKind::Direct
    };
    wasabi_domain::ChatSummary {
        id: candidate.chat.clone(),
        kind,
        display_name: Some(candidate.title.clone()),
        last_activity_ms: candidate.timestamp_ms,
        last_message_preview: Some(candidate.preview.clone()),
        unread_count: 1,
        pinned_at_ms: None,
        muted_until_ms: None,
        archived: false,
        favorite: false,
        draft_preview: None,
    }
}

fn typing_refresh_due(last_sent: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last_sent.is_none_or(|last| now.duration_since(last) >= TYPING_REFRESH_AFTER)
}

fn timeline_splice<T: PartialEq>(before: &[T], after: &[T]) -> (std::ops::Range<usize>, usize) {
    let prefix = before
        .iter()
        .zip(after.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let suffix_limit = before.len().min(after.len()).saturating_sub(prefix);
    let suffix = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take(suffix_limit)
        .take_while(|(left, right)| left == right)
        .count();
    (
        prefix..before.len().saturating_sub(suffix),
        after.len().saturating_sub(prefix + suffix),
    )
}

// ---- Watch appliers --------------------------------------------------------

/// Apply one session-state update plus derived side effects.
fn apply_state(
    weak: WeakEntity<MainWindow>,
    cx: &mut gpui::AsyncApp,
    state: wasabi_core::state::SessionState,
) -> Result<(), anyhow::Error> {
    weak.update(cx, |this, cx| {
        if state.is_connected() {
            this.session.connected_once = true;
            this.phone_pair_request_gen.fetch_add(1, Ordering::AcqRel);
            this.phone_pair_ticker_gen.fetch_add(1, Ordering::AcqRel);
            this.session.phone_pair_code = None;
            this.session.phone_pair_deadline = None;
            this.session.phone_pair_requesting = false;
            this.session.phone_pair_error = None;
        }
        if !state.is_connected() {
            this.typing.clear();
        }
        let left_pairing = this.session.qr_deadline.is_some()
            && !matches!(state, wasabi_core::state::SessionState::Pairing);
        if left_pairing {
            this.session.qr_code = None;
            this.session.qr_deadline = None;
            this.qr_ticker_gen.fetch_add(1, Ordering::AcqRel);
        }
        if matches!(
            &state,
            wasabi_core::state::SessionState::Connecting
                | wasabi_core::state::SessionState::Connected
        ) {
            this.pairing_request_gen.fetch_add(1, Ordering::AcqRel);
            this.session.pairing_requesting = false;
            this.session.pairing_error = None;
        }
        this.session.state = state;
        cx.notify();
    })
}

/// Apply one QR update and clear the request feedback once a code is ready.
fn apply_qr(
    weak: WeakEntity<MainWindow>,
    cx: &mut gpui::AsyncApp,
    qr: Option<wasabi_whatsapp::lifecycle::QrState>,
) -> Result<(), anyhow::Error> {
    weak.update(cx, |this, cx| {
        match qr {
            Some(qr) => {
                this.session.qr_code = Some(qr.code);
                this.session.qr_deadline = Some(std::time::Instant::now() + qr.expires_in);
                this.session.pairing_requesting = false;
                this.session.pairing_error = None;
                this.spawn_countdown_ticker(cx);
            }
            None => {
                let had_qr = this.session.qr_code.is_some() || this.session.qr_deadline.is_some();
                this.session.qr_code = None;
                this.session.qr_deadline = None;
                this.qr_ticker_gen.fetch_add(1, Ordering::AcqRel);
                if had_qr
                    && !this.session.pairing_requesting
                    && !this.session.use_phone_pairing
                    && matches!(
                        this.session.state,
                        wasabi_core::state::SessionState::Pairing
                    )
                {
                    this.restart_pairing(cx);
                }
            }
        }
        cx.notify();
    })
}

// ---- Spawn plumbing ---------------------------------------------------------

/// Detach one view-scoped task. Dropping the returned task handle is
/// intentional: GPUI cancels detached tasks when the view dies, and every
/// body re-checks its weak handle anyway.
pub(crate) fn spawn_main<F>(cx: &mut Context<'_, MainWindow>, f: F)
where
    F: AsyncFnOnce(WeakEntity<MainWindow>, &mut gpui::AsyncApp) + 'static,
{
    cx.spawn(f).detach();
}

#[cfg(test)]
mod tests {
    use super::{TYPING_REFRESH_AFTER, TypingDisplay, timeline_splice, typing_refresh_due};

    #[test]
    fn composing_updates_are_throttled_until_refresh_interval() {
        let now = std::time::Instant::now();
        assert!(typing_refresh_due(None, now));
        assert!(!typing_refresh_due(Some(now), now));
        assert!(typing_refresh_due(
            Some(now),
            now + TYPING_REFRESH_AFTER
        ));
    }

    #[test]
    fn incoming_typing_labels_distinguish_direct_group_and_recording() {
        let direct = TypingDisplay {
            state: wasabi_domain::TypingState::Composing,
            participant: None,
            generation: 1,
        };
        assert_eq!(direct.label(false), "typing…");

        let group = TypingDisplay {
            state: wasabi_domain::TypingState::RecordingAudio,
            participant: Some("alex@s.whatsapp.net".to_string()),
            generation: 1,
        };
        assert_eq!(group.label(true), "alex is recording audio…");
    }

    #[test]
    fn timeline_splices_only_changed_identity_range() {
        assert_eq!(timeline_splice(&["date", "m2"], &["date", "m1", "m2"]), (1..1, 1));
        assert_eq!(
            timeline_splice(&["date", "m1", "m2"], &["date", "m2", "m3"]),
            (1..3, 2)
        );
        assert_eq!(timeline_splice(&["date", "m1"], &["date", "m1"]), (2..2, 0));
    }
}
