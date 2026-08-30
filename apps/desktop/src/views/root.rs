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
    AnyWindowHandle, Context, FocusHandle, Focusable, Global, KeyBinding, ListAlignment, ListState,
    PathPromptOptions, Subscription, WeakEntity, Window, div, px,
};
use gpui_component::input::{InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, IconName, VirtualListScrollHandle};

use crate::core_bridge::DesktopBackend;
use crate::state::chats::ChatFilter;
use crate::state::{
    ChatListModel, DeviceSettings, MessageWindowModel, SessionMirror, SettingsSection,
};
use crate::theme;
use crate::views::{
    chat_list, composer, conversation, new_chat, new_group, pairing, right_panel, settings,
};
use wasabi_domain::{ChatKind, ChatScope, ConversationDetails, PendingMembershipRequest};

gpui::actions!(wasabi_desktop, [FocusSearch, OpenSettings, CloseInfo]);

pub const MAIN_KEY_CONTEXT: &str = "Main";
const CHAT_PAGE_LIMIT: usize = 100;
const MESSAGE_PAGE_LIMIT: usize = 60;
const SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);
const CONTACT_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
const CONTACT_PAGE_LIMIT: usize = 100;
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
    ConfirmChat(wasabi_domain::ChatAction),
    EditGroupText(GroupTextField),
    GroupMemberActions(GroupMemberTarget),
    ConfirmGroupMember(GroupMemberAction),
    ConfirmLeaveGroup(GroupLeaveTarget),
    ConfirmJoinRequest(JoinRequestAction),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupTextField {
    Subject,
    Description,
}

#[derive(Clone)]
pub(crate) struct GroupMemberTarget {
    pub(crate) chat: wasabi_domain::ChatId,
    pub(crate) group_name: String,
    pub(crate) participant: wasabi_domain::ChatId,
    pub(crate) participant_name: String,
    pub(crate) participant_role: wasabi_domain::ParticipantRole,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupMemberActionKind {
    Promote,
    Demote,
    Remove,
}

#[derive(Clone)]
pub(crate) struct GroupMemberAction {
    pub(crate) target: GroupMemberTarget,
    pub(crate) kind: GroupMemberActionKind,
}

#[derive(Clone)]
pub(crate) struct GroupLeaveTarget {
    pub(crate) chat: wasabi_domain::ChatId,
    pub(crate) group_name: String,
}

#[derive(Clone)]
pub(crate) struct JoinRequestTarget {
    pub(crate) chat: wasabi_domain::ChatId,
    pub(crate) group_name: String,
    pub(crate) participant: wasabi_domain::ChatId,
    pub(crate) participant_name: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinRequestActionKind {
    Approve,
    Decline,
}

#[derive(Clone)]
pub(crate) struct JoinRequestAction {
    pub(crate) target: JoinRequestTarget,
    pub(crate) kind: JoinRequestActionKind,
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
pub(crate) enum AvatarUi {
    Loading,
    Ready(std::path::PathBuf),
    Missing,
    Failed,
}

#[derive(Clone)]
pub(crate) enum PhoneLookupUi {
    Idle,
    Checking,
    Registered(wasabi_domain::ContactSummary),
    NotRegistered,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewChatMode {
    Direct,
    GroupParticipants,
    AddGroupMembers,
    GroupSubject,
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
                .map_or_else(
                    || action.to_string(),
                    |participant| format!("{participant} is {action}"),
                )
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
    pub(crate) group_mutation_in_progress: bool,
    pub(crate) group_mutation_error: Option<String>,
    pub(crate) group_mutation_feedback: Option<String>,
    pub(crate) group_leave_uncertain: bool,
    pub(crate) membership_requests: Vec<PendingMembershipRequest>,
    pub(crate) membership_requests_loading: bool,
    pub(crate) membership_requests_error: Option<String>,
    pub(crate) group_text_edit_error: Option<String>,
    pub(crate) settings: DeviceSettings,
    pub(crate) settings_section: SettingsSection,
    pub(crate) settings_overlay: Option<SettingsOverlay>,
    pub(crate) settings_feedback: Option<SettingsFeedback>,
    pub(crate) media_cache_usage_bytes: Option<u64>,
    pub(crate) media_cache_loading: bool,
    pub(crate) logout_in_progress: bool,
    pub(crate) send_error: Option<String>,
    pub(crate) active_draft: wasabi_domain::Draft,
    pub(crate) message_overlay: Option<MessageOverlay>,
    pub(crate) media_downloads:
        HashMap<(wasabi_domain::ChatId, wasabi_domain::MediaId), MediaDownloadUi>,
    pub(crate) avatars: HashMap<String, AvatarUi>,
    avatar_gens: HashMap<String, u64>,
    pub(crate) staged_attachments: HashMap<String, wasabi_domain::StagedAttachment>,
    pub(crate) attachment_staging: HashSet<String>,
    pub(crate) attachment_sending: HashSet<String>,
    pub(crate) retrying_messages: HashSet<(String, String)>,
    pub(crate) editing_messages: HashSet<(String, String)>,
    pub(crate) destructive_chats: HashSet<String>,
    pub(crate) new_chat_open: bool,
    pub(crate) contacts: Vec<wasabi_domain::ContactSummary>,
    pub(crate) contacts_next: Option<wasabi_domain::ContactPageCursor>,
    pub(crate) contacts_loading: bool,
    pub(crate) contacts_error: Option<String>,
    pub(crate) phone_lookup: PhoneLookupUi,
    pub(crate) new_chat_mode: NewChatMode,
    pub(crate) group_participants: Vec<wasabi_domain::ContactSummary>,
    pub(crate) group_creation_error: Option<String>,
    pub(crate) group_creating: bool,
    pub(crate) group_creation_uncertain: bool,
    pub(crate) composer_input: gpui::Entity<InputState>,
    pub(crate) search_input: gpui::Entity<InputState>,
    pub(crate) contact_search_input: gpui::Entity<InputState>,
    pub(crate) group_subject_input: gpui::Entity<InputState>,
    pub(crate) group_info_subject_input: gpui::Entity<InputState>,
    pub(crate) group_info_description_input: gpui::Entity<InputState>,
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
    contacts_gen: AtomicU64,
    phone_lookup_gen: AtomicU64,
    group_creation_gen: AtomicU64,
    messages_gen: AtomicU64,
    details_gen: AtomicU64,
    group_mutation_gen: AtomicU64,
    membership_requests_gen: AtomicU64,
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
    let mut bindings = vec![KeyBinding::new("escape", CloseInfo, Some(MAIN_KEY_CONTEXT))];
    if cfg!(target_os = "macos") {
        bindings.push(KeyBinding::new(
            "cmd-k",
            FocusSearch,
            Some(MAIN_KEY_CONTEXT),
        ));
        bindings.push(KeyBinding::new(
            "cmd-,",
            OpenSettings,
            Some(MAIN_KEY_CONTEXT),
        ));
    } else {
        bindings.push(KeyBinding::new(
            "ctrl-k",
            FocusSearch,
            Some(MAIN_KEY_CONTEXT),
        ));
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
        let contact_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search contacts"));
        let group_subject_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
        let group_info_subject_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
        let group_info_description_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 6)
                .placeholder("Add a group description")
        });
        let phone_pair_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Country code and phone number"));
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
            group_mutation_in_progress: false,
            group_mutation_error: None,
            group_mutation_feedback: None,
            group_leave_uncertain: false,
            membership_requests: Vec::new(),
            membership_requests_loading: false,
            membership_requests_error: None,
            group_text_edit_error: None,
            settings: DeviceSettings::load(),
            settings_section: SettingsSection::Chats,
            settings_overlay: None,
            settings_feedback: None,
            media_cache_usage_bytes: None,
            media_cache_loading: false,
            logout_in_progress: false,
            send_error: None,
            active_draft: wasabi_domain::Draft::default(),
            message_overlay: None,
            media_downloads: HashMap::new(),
            avatars: HashMap::new(),
            avatar_gens: HashMap::new(),
            staged_attachments: HashMap::new(),
            attachment_staging: HashSet::new(),
            attachment_sending: HashSet::new(),
            retrying_messages: HashSet::new(),
            editing_messages: HashSet::new(),
            destructive_chats: HashSet::new(),
            new_chat_open: false,
            contacts: Vec::new(),
            contacts_next: None,
            contacts_loading: false,
            contacts_error: None,
            phone_lookup: PhoneLookupUi::Idle,
            new_chat_mode: NewChatMode::Direct,
            group_participants: Vec::new(),
            group_creation_error: None,
            group_creating: false,
            group_creation_uncertain: false,
            composer_input,
            search_input,
            contact_search_input,
            group_subject_input,
            group_info_subject_input,
            group_info_description_input,
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
            notifications: crate::notifications::NotificationDispatcher::new(notification_click_tx),
            draft_generations: HashMap::new(),
            outbound_typing_generations: HashMap::new(),
            outbound_typing_sent_at: HashMap::new(),
            chats_gen: AtomicU64::new(0),
            search_gen: AtomicU64::new(0),
            contacts_gen: AtomicU64::new(0),
            phone_lookup_gen: AtomicU64::new(0),
            group_creation_gen: AtomicU64::new(0),
            messages_gen: AtomicU64::new(0),
            details_gen: AtomicU64::new(0),
            group_mutation_gen: AtomicU64::new(0),
            membership_requests_gen: AtomicU64::new(0),
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

        let on_contact_search_change = cx.subscribe_in(&this.contact_search_input, window, {
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && this.new_chat_open {
                    this.queue_contact_search(cx);
                }
            }
        });
        this.subscriptions.push(on_contact_search_change);

        let on_group_subject_change = cx.subscribe_in(&this.group_subject_input, window, {
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change)
                    && this.new_chat_mode == NewChatMode::GroupSubject
                {
                    let had_error = this.group_creation_error.take().is_some();
                    let was_uncertain = std::mem::take(&mut this.group_creation_uncertain);
                    if had_error || was_uncertain {
                        cx.notify();
                    }
                }
            }
        });
        this.subscriptions.push(on_group_subject_change);

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
                let mut draft = this.active_draft.clone();
                draft.body = body;
                draft.staged_attachments = staged_attachments;
                let draft = (!draft.body.trim().is_empty()
                    || !draft.staged_attachments.is_empty()
                    || draft.reply_to.is_some()
                    || draft.edit_target.is_some())
                .then_some(draft);
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
            this.install_preview(&preview_mode, window, cx);
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
    fn install_preview(&mut self, mode: &str, window: &mut Window, cx: &mut Context<Self>) {
        let preview = crate::state::preview::media_preview();
        self.session.state = wasabi_core::state::SessionState::Connected;
        self.session.connected_once = true;
        self.chats.loading = false;
        self.chats.selected = Some(preview.chat.as_str().to_string());
        self.chats.chats = vec![preview.summary];
        self.messages.chat_id = Some(preview.chat.as_str().to_string());
        self.messages.anchor_newest(&preview.page);
        if mode == "retry"
            && let Some(row) = self
                .messages
                .rows
                .iter_mut()
                .find(|row| row.direction == wasabi_domain::MessageDirection::Outgoing)
        {
            row.status = wasabi_domain::MessageStatus::Failed;
            self.messages.rebuild();
        }
        if mode == "reply" {
            let quoted = wasabi_domain::QuotedMessage {
                id: wasabi_domain::MessageId::new("PREVIEW-DOC"),
                sender: Some("Avery Chen".to_string()),
                preview: "Quarterly report.pdf".to_string(),
            };
            if let Some(row) = self
                .messages
                .rows
                .iter_mut()
                .find(|row| row.id.as_str() == "PREVIEW-MULTILINE")
            {
                row.quoted = Some(quoted);
            }
            self.active_draft.reply_to = Some(wasabi_domain::MessageId::new("PREVIEW-DOC"));
            self.messages.rebuild();
        }
        if mode == "edit"
            && let Some(row) = self
                .messages
                .rows
                .iter_mut()
                .find(|row| row.id.as_str() == "PREVIEW-TEXT")
        {
            let original = crate::state::messages::body_text(row);
            row.edited_at_ms = Some(chrono::Utc::now().timestamp_millis());
            self.active_draft.body = format!("{original} I’ll send notes before lunch.");
            self.active_draft.edit_target = Some(row.id.clone());
            self.composer_input.update(cx, |input, cx| {
                composer::set_text_at_end(input, self.active_draft.body.clone(), window, cx)
            });
            self.messages.rebuild();
        }
        if mode == "reactions"
            && let Some(row) = self
                .messages
                .rows
                .iter_mut()
                .find(|row| row.id.as_str() == "PREVIEW-MULTILINE")
        {
            row.reactions = vec![
                wasabi_domain::ReactionSummary {
                    emoji: "👍".to_string(),
                    count: 4,
                    reacted_by_me: true,
                },
                wasabi_domain::ReactionSummary {
                    emoji: "❤️".to_string(),
                    count: 2,
                    reacted_by_me: false,
                },
                wasabi_domain::ReactionSummary {
                    emoji: "🎉".to_string(),
                    count: 1,
                    reacted_by_me: false,
                },
            ];
            self.messages.rebuild();
        }
        if matches!(mode, "composer-multiline" | "composer-multiline-large") {
            let body = "First line of a real multiline draft.\nSecond line wraps naturally on compact windows.\nمرحبا — 日本語 — emoji 🎉 remain editable."
                .to_string();
            self.active_draft.body = body.clone();
            self.composer_input.update(cx, |input, cx| {
                composer::set_text_at_end(input, body, window, cx)
            });
        }
        self.msg_scroll.reset(self.messages.items.len());
        self.msg_scroll.scroll_to_end();
        if mode == "reactions" {
            self.msg_scroll.remeasure();
        }
        if mode == "media" {
            self.staged_attachments.insert(
                preview.chat.as_str().to_string(),
                wasabi_domain::StagedAttachment {
                    transfer: wasabi_domain::TransferId::new("preview-transfer"),
                    kind: wasabi_domain::AttachmentKind::Document,
                    display_name: "wasabi product brief.pdf".to_string(),
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
        if matches!(mode, "timeline-large" | "composer-multiline-large") {
            self.settings.text_scale = 150;
            self.msg_scroll.remeasure();
        }
        if mode == "timeline-pending" {
            self.pending_new_messages = 3;
            self.near_bottom = false;
        }
        if mode == "chat-actions" {
            self.show_right_panel = true;
        }
        if matches!(
            mode,
            "group-info"
                | "group-description-edit"
                | "group-add-members"
                | "group-member-actions"
                | "group-member-remove"
                | "group-leave"
        ) {
            let group_chat = "preview-group@g.us";
            if let Some(summary) = self.chats.chats.first_mut() {
                summary.id = wasabi_domain::ChatId::new(group_chat);
                summary.kind = ChatKind::Group;
                summary.display_name = Some("Weekend hiking crew".to_string());
            }
            self.chats.selected = Some(group_chat.to_string());
            self.conversation_details = Some(ConversationDetails::Group(
                crate::state::preview::group_details_preview(),
            ));
            self.show_right_panel = true;
            if mode == "group-description-edit" {
                self.begin_group_text_edit(
                    GroupTextField::Description,
                    "Trail plans, weather checks, and shared packing lists.".to_string(),
                    window,
                    cx,
                );
            } else if mode == "group-leave" {
                let details = crate::state::preview::group_details_preview();
                self.message_overlay = Some(MessageOverlay::ConfirmLeaveGroup(GroupLeaveTarget {
                    chat: details.chat,
                    group_name: details.subject,
                }));
            } else if matches!(mode, "group-member-actions" | "group-member-remove") {
                let details = crate::state::preview::group_details_preview();
                if let Some(participant) = details
                    .participants
                    .iter()
                    .find(|participant| participant.display_name == "Avery Chen")
                {
                    let target = GroupMemberTarget {
                        chat: details.chat.clone(),
                        group_name: details.subject.clone(),
                        participant: wasabi_domain::ChatId::new(participant.jid.clone()),
                        participant_name: participant.display_name.clone(),
                        participant_role: participant.role,
                    };
                    self.message_overlay = Some(if mode == "group-member-remove" {
                        MessageOverlay::ConfirmGroupMember(GroupMemberAction {
                            target,
                            kind: GroupMemberActionKind::Remove,
                        })
                    } else {
                        MessageOverlay::GroupMemberActions(target)
                    });
                }
            }
        }
        if matches!(mode, "chat-clear" | "chat-delete") {
            let chat = preview.chat.clone();
            self.message_overlay = Some(MessageOverlay::ConfirmChat(if mode == "chat-clear" {
                wasabi_domain::ChatAction::Clear {
                    chat,
                    delete_starred: false,
                    delete_media: false,
                }
            } else {
                wasabi_domain::ChatAction::Delete {
                    chat,
                    delete_media: false,
                }
            }));
        }
        if matches!(
            mode,
            "new-chat"
                | "new-chat-phone"
                | "new-group-participants"
                | "new-group-subject"
                | "new-group-creating"
                | "group-add-members"
        ) {
            self.new_chat_open = true;
            self.contacts = [
                ("Amara Okafor", "15550000001@s.whatsapp.net"),
                ("Avery Chen", "15550000002@s.whatsapp.net"),
                ("Diego Morales", "15550000003@s.whatsapp.net"),
                ("Fatima Zahra", "15550000004@s.whatsapp.net"),
                ("Priya Sharma", "15550000005@s.whatsapp.net"),
                ("佐藤 美咲", "15550000006@s.whatsapp.net"),
            ]
            .into_iter()
            .map(|(display_name, jid)| wasabi_domain::ContactSummary {
                jid: wasabi_domain::ChatId::new(jid),
                display_name: display_name.to_string(),
                phone_number: jid.split('@').next().map(str::to_string),
                avatar: None,
            })
            .collect();
            if mode == "new-chat-phone" {
                let contact = wasabi_domain::ContactSummary {
                    jid: wasabi_domain::ChatId::new("15551234567@s.whatsapp.net"),
                    display_name: "Northwind Coffee".to_string(),
                    phone_number: Some("15551234567".to_string()),
                    avatar: None,
                };
                self.phone_lookup = PhoneLookupUi::Registered(contact);
                self.contact_search_input.update(cx, |input, cx| {
                    input.set_value("+1 (555) 123-4567", window, cx)
                });
                self.contacts.clear();
            } else if mode == "group-add-members" {
                self.contacts = [
                    ("Avery Chen", "preview-avery@s.whatsapp.net"),
                    ("Amara Okafor", "preview-amara@s.whatsapp.net"),
                    ("Fatima Zahra", "preview-fatima@s.whatsapp.net"),
                    ("Priya Sharma", "preview-priya@s.whatsapp.net"),
                    ("佐藤 美咲", "preview-misaki@s.whatsapp.net"),
                ]
                .into_iter()
                .map(|(display_name, jid)| wasabi_domain::ContactSummary {
                    jid: wasabi_domain::ChatId::new(jid),
                    display_name: display_name.to_string(),
                    phone_number: None,
                    avatar: None,
                })
                .collect();
                self.new_chat_mode = NewChatMode::AddGroupMembers;
                self.group_participants = self.contacts.iter().skip(2).take(2).cloned().collect();
            } else if matches!(
                mode,
                "new-group-participants" | "new-group-subject" | "new-group-creating"
            ) {
                self.group_participants = self.contacts.iter().take(3).cloned().collect();
                self.new_chat_mode = if mode == "new-group-participants" {
                    NewChatMode::GroupParticipants
                } else {
                    NewChatMode::GroupSubject
                };
                if self.new_chat_mode == NewChatMode::GroupSubject {
                    self.group_subject_input.update(cx, |input, cx| {
                        input.set_value("Weekend hiking crew", window, cx)
                    });
                }
                self.group_creating = mode == "new-group-creating";
            }
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

    pub(crate) fn open_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_chat_open = true;
        self.new_chat_mode = NewChatMode::Direct;
        self.group_participants.clear();
        self.group_creation_error = None;
        self.group_creating = false;
        self.group_creation_uncertain = false;
        self.group_creation_gen.fetch_add(1, Ordering::AcqRel);
        self.contacts.clear();
        self.contacts_next = None;
        self.contacts_error = None;
        self.phone_lookup = PhoneLookupUi::Idle;
        self.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
        self.contact_search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.load_contact_query(false, cx);
        self.contact_search_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn close_new_chat(&mut self, cx: &mut Context<Self>) {
        if self.new_chat_open {
            self.new_chat_open = false;
            self.contacts_gen.fetch_add(1, Ordering::AcqRel);
            self.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
            self.contacts_loading = false;
            self.phone_lookup = PhoneLookupUi::Idle;
            self.new_chat_mode = NewChatMode::Direct;
            self.group_participants.clear();
            self.group_creation_error = None;
            self.group_creating = false;
            self.group_creation_uncertain = false;
            self.group_creation_gen.fetch_add(1, Ordering::AcqRel);
            cx.notify();
        }
    }

    pub(crate) fn begin_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_chat_mode = NewChatMode::GroupParticipants;
        self.group_participants.clear();
        self.group_creation_error = None;
        self.group_creating = false;
        self.group_creation_uncertain = false;
        self.group_creation_gen.fetch_add(1, Ordering::AcqRel);
        self.contact_search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.load_contact_query(false, cx);
        self.contact_search_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn begin_add_group_members(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            return;
        };
        if !details.permissions.can_manage_members()
            || self.group_mutation_in_progress
            || self.group_leave_uncertain
        {
            return;
        }
        self.new_chat_open = true;
        self.new_chat_mode = NewChatMode::AddGroupMembers;
        self.group_participants.clear();
        self.group_creation_error = None;
        self.group_creating = false;
        self.group_creation_uncertain = false;
        self.group_creation_gen.fetch_add(1, Ordering::AcqRel);
        self.contacts.clear();
        self.contacts_next = None;
        self.contacts_error = None;
        self.group_mutation_feedback = None;
        self.contact_search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.load_contact_query(false, cx);
        self.contact_search_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn toggle_group_participant(
        &mut self,
        contact: wasabi_domain::ContactSummary,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self
            .group_participants
            .iter()
            .position(|selected| selected.jid == contact.jid)
        {
            self.group_participants.remove(index);
        } else if self.group_participants.len() < wasabi_domain::GROUP_INVITEE_MAX {
            self.group_participants.push(contact);
        } else {
            self.group_creation_error =
                Some("A group can include up to 256 invited participants.".to_string());
        }
        self.group_creation_uncertain = false;
        cx.notify();
    }

    pub(crate) fn continue_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.group_participants.is_empty() {
            self.group_creation_error = Some("Select at least one participant.".to_string());
            cx.notify();
            return;
        }
        self.new_chat_mode = NewChatMode::GroupSubject;
        self.group_creation_error = None;
        self.group_creation_uncertain = false;
        self.group_subject_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.group_subject_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn submit_add_group_members(&mut self, cx: &mut Context<Self>) {
        if self.group_creating || self.new_chat_mode != NewChatMode::AddGroupMembers {
            return;
        }
        if self.group_creation_uncertain {
            self.group_creation_error = Some(
                "Refresh the group before retrying so the same people are not added twice."
                    .to_string(),
            );
            cx.notify();
            return;
        }
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            self.group_creation_error =
                Some("Group information is no longer available.".to_string());
            cx.notify();
            return;
        };
        if !details.permissions.can_manage_members() {
            self.group_creation_error = Some("Only group admins can add members.".to_string());
            cx.notify();
            return;
        }
        if !self.session.state.is_connected() {
            self.group_creation_error = Some("Reconnect before adding group members.".to_string());
            cx.notify();
            return;
        }
        let patch = match wasabi_domain::GroupPatch::add_participants(
            details.chat.clone(),
            self.group_participants
                .iter()
                .map(|contact| contact.jid.clone())
                .collect(),
        ) {
            Ok(patch) => patch,
            Err(error) => {
                self.group_creation_error = Some(format!("{error}."));
                cx.notify();
                return;
            }
        };
        let target = patch.chat().as_str().to_string();
        let generation = self.group_creation_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.group_creating = true;
        self.group_creation_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.update_group(patch).await;
            this.update(cx, |this, cx| {
                if !this.new_chat_open
                    || this.new_chat_mode != NewChatMode::AddGroupMembers
                    || this.group_creation_gen.load(Ordering::Acquire) != generation
                {
                    return;
                }
                this.group_creating = false;
                if this.chats.selected.as_deref() != Some(target.as_str())
                    || !this.show_right_panel
                {
                    this.close_new_chat(cx);
                    return;
                }
                match result {
                    Ok(result) => {
                        let applied = result.applied_participants;
                        let rejected = result.rejected_participants;
                        if let Some(details) = result.details {
                            this.conversation_details = Some(ConversationDetails::Group(details));
                        }
                        this.group_mutation_feedback = Some(if applied == 0 {
                            "No members were added. The group list was refreshed.".to_string()
                        } else if rejected == 0 {
                            format!(
                                "Added {applied} {}.",
                                if applied == 1 { "member" } else { "members" }
                            )
                        } else {
                            format!(
                                "Added {applied}; {rejected} could not be added. The group list was refreshed."
                            )
                        });
                        this.close_new_chat(cx);
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "adding group members failed");
                        let (message, uncertain) = group_member_add_failure(error.kind);
                        this.group_creation_error = Some(message);
                        this.group_creation_uncertain = uncertain;
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn back_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.group_creating {
            return;
        }
        self.group_creation_error = None;
        self.group_creation_uncertain = false;
        match self.new_chat_mode {
            NewChatMode::GroupSubject => {
                self.new_chat_mode = NewChatMode::GroupParticipants;
                self.contact_search_input
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            NewChatMode::GroupParticipants => {
                self.new_chat_mode = NewChatMode::Direct;
                self.group_participants.clear();
            }
            NewChatMode::AddGroupMembers => self.close_new_chat(cx),
            NewChatMode::Direct => {}
        }
        cx.notify();
    }

    pub(crate) fn submit_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.group_creating || self.new_chat_mode != NewChatMode::GroupSubject {
            return;
        }
        if self.group_creation_uncertain {
            self.group_creation_error = Some(
                "Check Chats for the group, or change its name before creating again.".to_string(),
            );
            cx.notify();
            return;
        }
        let subject = self.group_subject_input.read(cx).value().to_string();
        let request = match wasabi_domain::CreateGroupRequest::new(
            subject,
            self.group_participants
                .iter()
                .map(|contact| contact.jid.clone())
                .collect(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.group_creation_error = Some(format!("{error}."));
                cx.notify();
                return;
            }
        };
        if !self.session.state.is_connected() {
            self.group_creation_error = Some("Reconnect before creating this group.".to_string());
            cx.notify();
            return;
        }

        let generation = self.group_creation_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.group_creating = true;
        self.group_creation_uncertain = false;
        self.group_creation_error = None;
        let bridge = Arc::clone(&self.bridge);
        let window_handle = window.window_handle();
        spawn_main(cx, async move |this, cx| {
            let result = bridge.create_group(request).await;
            let navigation = this.update(cx, |state, cx| {
                if !state.new_chat_open
                    || state.new_chat_mode != NewChatMode::GroupSubject
                    || state.group_creation_gen.load(Ordering::Acquire) != generation
                {
                    return None;
                }
                state.group_creating = false;
                match result {
                    Ok(details) => {
                        let chat = details.chat.as_str().to_string();
                        state.new_chat_open = false;
                        state.new_chat_mode = NewChatMode::Direct;
                        state.group_participants.clear();
                        state.contacts_gen.fetch_add(1, Ordering::AcqRel);
                        state.refresh_chats(cx);
                        cx.notify();
                        Some((chat, details))
                    }
                    Err(error) => {
                        let (message, uncertain) = group_creation_failure(error.kind);
                        state.group_creation_error = Some(message);
                        state.group_creation_uncertain = uncertain;
                        cx.notify();
                        None
                    }
                }
            });
            if let Ok(Some((chat, details))) = navigation {
                window_handle
                    .update(cx, |_, window, cx| {
                        this.update(cx, |state, cx| {
                            state.open_chat(chat, None, window, cx);
                            state.conversation_details =
                                Some(wasabi_domain::ConversationDetails::Group(details));
                        })
                        .ok();
                    })
                    .ok();
            }
        });
        cx.notify();
    }

    fn queue_contact_search(&mut self, cx: &mut Context<Self>) {
        self.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
        self.phone_lookup = PhoneLookupUi::Idle;
        if matches!(
            self.new_chat_mode,
            NewChatMode::GroupParticipants | NewChatMode::AddGroupMembers
        ) {
            self.group_creation_error = None;
        }
        self.load_contact_query(true, cx);
    }

    pub(crate) fn lookup_phone_contact(&mut self, cx: &mut Context<Self>) {
        let input = self.contact_search_input.read(cx).value().to_string();
        let Ok(phone) = wasabi_domain::ContactPhoneNumber::parse(&input) else {
            return;
        };
        if !self.session.state.is_connected() {
            self.phone_lookup = PhoneLookupUi::Failed(
                "Connect to check whether this number has an account.".to_string(),
            );
            cx.notify();
            return;
        }

        let generation = self.phone_lookup_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.phone_lookup = PhoneLookupUi::Checking;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.lookup_contact(phone).await;
            this.update(cx, |this, cx| {
                if !this.new_chat_open
                    || this.phone_lookup_gen.load(Ordering::Acquire) != generation
                {
                    return;
                }
                this.phone_lookup = match result {
                    Ok(wasabi_domain::ContactLookupResult::Registered(contact)) => {
                        PhoneLookupUi::Registered(contact)
                    }
                    Ok(wasabi_domain::ContactLookupResult::NotRegistered) => {
                        PhoneLookupUi::NotRegistered
                    }
                    Err(error) => PhoneLookupUi::Failed(match error.kind {
                        wasabi_domain::ErrorKind::RateLimited => {
                            "Too many checks. Wait a little, then try again.".to_string()
                        }
                        wasabi_domain::ErrorKind::Timeout => {
                            "The check timed out. Try again.".to_string()
                        }
                        wasabi_domain::ErrorKind::NotConnected => {
                            "Connection lost. Reconnect, then try again.".to_string()
                        }
                        _ => "Couldn’t check this number. Try again.".to_string(),
                    }),
                };
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn load_contact_query(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let generation = self.contacts_gen.fetch_add(1, Ordering::AcqRel) + 1;
        let query = normalized_contact_query(&self.contact_search_input.read(cx).value());
        self.contacts.clear();
        self.contacts_next = None;
        self.contacts_error = None;
        self.contacts_loading = true;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            if debounce {
                cx.background_executor()
                    .timer(CONTACT_SEARCH_DEBOUNCE)
                    .await;
            }
            let current = this
                .update(cx, |this, _| {
                    this.new_chat_open && this.contacts_gen.load(Ordering::Acquire) == generation
                })
                .unwrap_or(false);
            if !current {
                return;
            }
            let result = bridge.contact_page(query, None, CONTACT_PAGE_LIMIT).await;
            this.update(cx, |this, cx| {
                if !this.new_chat_open || this.contacts_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                this.contacts_loading = false;
                match result {
                    Ok(page) => {
                        this.contacts = page.rows;
                        this.contacts_next = page.next_after;
                    }
                    Err(error) => this.contacts_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn load_more_contacts(&mut self, cx: &mut Context<Self>) {
        let Some(after) = self.contacts_next.clone() else {
            return;
        };
        if self.contacts_loading || !self.new_chat_open {
            return;
        }
        let generation = self.contacts_gen.load(Ordering::Acquire);
        let query = normalized_contact_query(&self.contact_search_input.read(cx).value());
        self.contacts_loading = true;
        self.contacts_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge
                .contact_page(query, Some(after), CONTACT_PAGE_LIMIT)
                .await;
            this.update(cx, |this, cx| {
                if !this.new_chat_open || this.contacts_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                this.contacts_loading = false;
                match result {
                    Ok(page) => {
                        for contact in page.rows {
                            if !this.contacts.iter().any(|row| row.jid == contact.jid) {
                                this.contacts.push(contact);
                            }
                        }
                        this.contacts_next = page.next_after;
                    }
                    Err(error) => this.contacts_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn start_contact_chat(
        &mut self,
        contact: wasabi_domain::ContactSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chat_id = contact.jid.as_str().to_string();
        if self.chats.scope != wasabi_domain::ChatScope::Active {
            self.switch_chat_scope(wasabi_domain::ChatScope::Active, cx);
        } else {
            self.chats.filter = ChatFilter::All;
        }
        self.new_chat_open = false;
        self.contacts_gen.fetch_add(1, Ordering::AcqRel);
        self.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
        self.phone_lookup = PhoneLookupUi::Idle;
        self.open_chat(chat_id, None, window, cx);
        self.conversation_details = Some(wasabi_domain::ConversationDetails::Direct(
            wasabi_domain::DirectContactDetails {
                jid: contact.jid.as_str().to_string(),
                display_name: contact.display_name,
                phone_number: contact.phone_number,
                about: None,
                avatar: contact.avatar,
            },
        ));
        cx.notify();
    }

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
        self.group_mutation_gen.fetch_add(1, Ordering::AcqRel);
        self.show_right_panel = false;
        self.message_overlay = None;
        self.active_draft = wasabi_domain::Draft::default();
        self.conversation_details = None;
        self.details_loading = false;
        self.details_error = None;
        self.group_mutation_in_progress = false;
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.group_leave_uncertain = false;
        self.reset_membership_requests();
        self.refresh_chats(cx);
    }

    fn select_nav(&mut self, destination: NavDestination, cx: &mut Context<Self>) {
        self.new_chat_open = false;
        self.contacts_gen.fetch_add(1, Ordering::AcqRel);
        self.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
        self.phone_lookup = PhoneLookupUi::Idle;
        self.new_chat_mode = NewChatMode::Direct;
        self.group_participants.clear();
        self.group_creation_error = None;
        self.group_creating = false;
        self.group_creation_uncertain = false;
        self.group_creation_gen.fetch_add(1, Ordering::AcqRel);
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
        self.group_mutation_gen.fetch_add(1, Ordering::AcqRel);
        self.chats.selected = Some(chat_id.clone());
        self.messages.reset_for_chat(&chat_id);
        self.msg_scroll.reset(0);
        self.show_right_panel = false;
        self.message_overlay = None;
        self.conversation_details = None;
        self.details_loading = false;
        self.details_error = None;
        self.group_mutation_in_progress = false;
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.group_leave_uncertain = false;
        self.reset_membership_requests();
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
            .and_then(|chat| chat.draft.clone())
            .unwrap_or_default();
        let draft_body = draft.body.clone();
        self.active_draft = draft;
        self.composer_input.update(cx, |input, cx| {
            composer::set_text_at_end(input, draft_body, window, cx)
        });
        if self
            .chats
            .chats
            .iter()
            .find(|chat| chat.id.as_str() == chat_id)
            .is_some_and(|chat| matches!(chat.kind, ChatKind::Direct | ChatKind::Group))
        {
            self.request_avatar(chat_id.clone(), false, cx);
        }
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
        self.active_draft.body = body.clone();
        self.active_draft.staged_attachments = staged_attachments.clone();
        let draft = (!body.trim().is_empty()
            || !staged_attachments.is_empty()
            || self.active_draft.reply_to.is_some()
            || self.active_draft.edit_target.is_some())
        .then(|| self.active_draft.clone());
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
                    .or_else(|| {
                        self.active_draft
                            .reply_to
                            .as_ref()
                            .map(|_| "Replying to a message".to_string())
                    })
            };
            summary.draft = draft.clone();
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

    fn spawn_typing_expiry(&mut self, chat: String, generation: u64, cx: &mut Context<Self>) {
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
        if self.active_draft.edit_target.is_some() {
            self.submit_current_edit(chat_id, window, cx);
            return;
        }
        let text = self.composer_input.read(cx).value().trim().to_string();
        let reply_to = self.active_draft.reply_to.clone();
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
                // Clear optimistically; the durable row arrives through an
                // invalidation and a failed row retains its same-ID retry.
                self.composer_input
                    .update(cx, |state, cx| state.set_value("", window, cx));
                reply_to.clone().map_or_else(
                    || {
                        wasabi_domain::SendRequest::text(
                            wasabi_domain::ChatId::new(chat_id.clone()),
                            text.clone(),
                        )
                    },
                    |reply_to| {
                        wasabi_domain::SendRequest::reply(
                            wasabi_domain::ChatId::new(chat_id.clone()),
                            text.clone(),
                            reply_to,
                        )
                    },
                )
            },
            |attachment| {
                let caption =
                    (attachment.kind != wasabi_domain::AttachmentKind::Audio).then(|| text.clone());
                reply_to.clone().map_or_else(
                    || {
                        wasabi_domain::SendRequest::attachment(
                            wasabi_domain::ChatId::new(chat_id.clone()),
                            attachment.transfer.clone(),
                            caption.clone(),
                        )
                    },
                    |reply_to| {
                        wasabi_domain::SendRequest::attachment_reply(
                            wasabi_domain::ChatId::new(chat_id.clone()),
                            attachment.transfer.clone(),
                            caption.clone(),
                            reply_to,
                        )
                    },
                )
            },
        );
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let primary_result = bridge.send(request).await;
            let primary_accepted = primary_result.is_ok();
            let mut result = primary_result;
            if primary_accepted
                && attachment.as_ref().is_some_and(|attachment| {
                    attachment.kind == wasabi_domain::AttachmentKind::Audio
                })
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
            let text_only = attachment.is_none();
            let transfer = attachment
                .as_ref()
                .map(|attachment| attachment.transfer.clone());
            let update = this.update(cx, |this, cx| {
                this.attachment_sending.remove(&chat_id);
                if primary_accepted
                    && transfer.as_ref().is_some_and(|transfer| {
                        this.staged_attachments
                            .get(&chat_id)
                            .is_some_and(|current| current.transfer == *transfer)
                    })
                {
                    this.staged_attachments.remove(&chat_id);
                }
                if primary_accepted
                    && this.chats.selected.as_deref() == Some(chat_id.as_str())
                    && this.active_draft.reply_to == reply_to
                    && {
                        let current = this.composer_input.read(cx).value().trim().to_string();
                        current.is_empty() || current == text
                    }
                {
                    this.active_draft.reply_to = None;
                    this.queue_draft_save(cx);
                }
                if let Err(error) = &result {
                    tracing::warn!(kind = %error.kind, "send failed");
                    this.send_error = Some(error.ui_message().to_string());
                }
                cx.notify();
                (this.window_handle, this.composer_input.clone())
            });
            if let Ok((window_handle, input)) = update {
                window_handle
                    .update(cx, |_, window, cx| {
                        input.update(cx, |state, cx| {
                            if accepted && state.value().trim() == text {
                                state.set_value("", window, cx);
                            } else if should_restore_composer(accepted, text_only, &state.value()) {
                                // This failure happened before a durable row
                                // was accepted, so there is no bubble Retry to
                                // own the user's text.
                                composer::set_text_at_end(state, text.clone(), window, cx);
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

    pub(crate) fn avatar_path(&self, jid: &str) -> Option<&std::path::Path> {
        match self.avatars.get(jid) {
            Some(AvatarUi::Ready(path)) => Some(path.as_path()),
            _ => None,
        }
    }

    pub(crate) fn request_avatar(&mut self, jid: String, refresh: bool, cx: &mut Context<Self>) {
        if jid.is_empty() {
            return;
        }
        if !refresh
            && matches!(
                self.avatars.get(&jid),
                Some(AvatarUi::Loading | AvatarUi::Ready(_) | AvatarUi::Missing)
            )
        {
            return;
        }
        if !matches!(self.avatars.get(&jid), Some(AvatarUi::Ready(_))) {
            self.avatars.insert(jid.clone(), AvatarUi::Loading);
        }
        let generation = {
            let generation = self.avatar_gens.entry(jid.clone()).or_insert(0);
            *generation += 1;
            *generation
        };
        let request = wasabi_domain::ProfilePictureRequest {
            jid: wasabi_domain::ChatId::new(jid.clone()),
            refresh,
        };
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.profile_picture(request).await;
            this.update(cx, |this, cx| {
                if this.avatar_gens.get(&jid) != Some(&generation) {
                    return;
                }
                match result {
                    Ok(Some(cached)) => {
                        this.avatars.insert(jid, AvatarUi::Ready(cached.path));
                    }
                    Ok(None) => {
                        this.avatars.insert(jid, AvatarUi::Missing);
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "profile picture download failed");
                        if !matches!(this.avatars.get(&jid), Some(AvatarUi::Ready(_))) {
                            this.avatars.insert(jid, AvatarUi::Failed);
                        }
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
        let request_generation = self.phone_pair_request_gen.fetch_add(1, Ordering::AcqRel) + 1;
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
        self.group_mutation_gen.fetch_add(1, Ordering::AcqRel);
        self.show_right_panel = false;
        self.details_loading = false;
        self.group_mutation_in_progress = false;
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.group_leave_uncertain = false;
        self.reset_membership_requests();
        cx.notify();
    }

    fn load_conversation_details(&mut self, cx: &mut Context<Self>) {
        let Some((chat, kind)) = self.chats.selected.as_ref().and_then(|selected| {
            self.chats
                .chats
                .iter()
                .find(|summary| summary.id.as_str() == selected)
                .map(|summary| (selected.clone(), summary.kind))
        }) else {
            self.details_error = Some("Conversation information is unavailable".to_string());
            cx.notify();
            return;
        };
        let generation = self.details_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.conversation_details = None;
        self.details_loading = true;
        self.details_error = None;
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.group_leave_uncertain = false;
        self.reset_membership_requests();
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
                if this.details_gen.load(Ordering::Acquire) != generation || !this.show_right_panel
                {
                    return;
                }
                this.details_loading = false;
                match result {
                    Ok(details) => {
                        let avatar_jid = match &details {
                            ConversationDetails::Direct(contact) => contact.jid.clone(),
                            ConversationDetails::Group(group) => group.chat.as_str().to_string(),
                        };
                        this.conversation_details = Some(details);
                        this.request_avatar(avatar_jid, false, cx);
                        this.load_membership_requests(cx);
                    }
                    Err(error) => this.details_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn apply_group_patch(
        &mut self,
        patch: wasabi_domain::GroupPatch,
        cx: &mut Context<Self>,
    ) {
        if self.group_mutation_in_progress || self.group_leave_uncertain {
            return;
        }
        let target = patch.chat().as_str().to_string();
        let success_feedback = match patch.change() {
            wasabi_domain::GroupChange::PromoteParticipant(_) => {
                Some("Participant is now a group admin.".to_string())
            }
            wasabi_domain::GroupChange::DemoteParticipant(_) => {
                Some("Participant is no longer a group admin.".to_string())
            }
            wasabi_domain::GroupChange::RemoveParticipant(_) => {
                Some("Participant was removed from the group.".to_string())
            }
            wasabi_domain::GroupChange::ApproveMembershipRequest(_) => {
                Some("Join request approved.".to_string())
            }
            wasabi_domain::GroupChange::RejectMembershipRequest(_) => {
                Some("Join request declined.".to_string())
            }
            _ => None,
        };
        let reviewed_request = match patch.change() {
            wasabi_domain::GroupChange::ApproveMembershipRequest(jid)
            | wasabi_domain::GroupChange::RejectMembershipRequest(jid) => Some(jid.clone()),
            _ => None,
        };
        let leaving = matches!(patch.change(), wasabi_domain::GroupChange::Leave);
        let generation = self.group_mutation_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.group_mutation_in_progress = true;
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.update_group(patch).await;
            this.update(cx, |this, cx| {
                if this.group_mutation_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                this.group_mutation_in_progress = false;
                if this.chats.selected.as_deref() != Some(target.as_str())
                    || !this.show_right_panel
                {
                    return;
                }
                match result {
                    Ok(result) => {
                        this.group_mutation_error = None;
                        this.group_mutation_feedback = if result.rejected_participants > 0 {
                            this.group_mutation_error = Some(
                                "The linked account did not apply this participant change. The group list was refreshed."
                                    .to_string(),
                            );
                            None
                        } else {
                            success_feedback
                        };
                        if result.applied_participants > 0
                            && let Some(jid) = reviewed_request
                        {
                            this.membership_requests
                                .retain(|request| request.jid != jid);
                        }
                        if let Some(details) = result.details {
                            this.conversation_details = Some(ConversationDetails::Group(details));
                        } else {
                            this.close_right_panel(cx);
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "group mutation failed");
                        if leaving && crate::core_bridge::leave_outcome_uncertain(error.kind)
                        {
                            this.group_leave_uncertain = true;
                            this.group_mutation_error = Some(
                                "The leave result could not be confirmed. Reconnect and reopen group info before trying again."
                                    .to_string(),
                            );
                        } else {
                            this.group_mutation_error = Some(error.ui_message().to_string());
                        }
                    }
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
        let reaction_change = match &action {
            wasabi_domain::MessageAction::React { emoji, .. } => self
                .messages
                .rows
                .iter()
                .find(|row| row.chat == target.chat && row.id == target.message)
                .map(|row| {
                    let previous = row.reactions.clone();
                    let optimistic = optimistic_own_reaction(&previous, emoji);
                    (previous, optimistic)
                }),
            _ => None,
        };
        let retry_key = matches!(action, wasabi_domain::MessageAction::Retry { .. }).then(|| {
            (
                target.chat.as_str().to_string(),
                target.message.as_str().to_string(),
            )
        });
        if let Some(key) = retry_key.clone() {
            if !self.retrying_messages.insert(key) {
                return;
            }
            self.message_overlay = None;
            self.send_error = None;
        }
        if let Some(starred) = desired_star
            && let Some(row) = self
                .messages
                .rows
                .iter_mut()
                .find(|row| row.chat == target.chat && row.id == target.message)
        {
            row.starred = starred;
        }
        if let Some((_, optimistic)) = &reaction_change
            && let Some(row) = self
                .messages
                .rows
                .iter_mut()
                .find(|row| row.chat == target.chat && row.id == target.message)
        {
            row.reactions = optimistic.clone();
            self.messages.rebuild();
            self.msg_scroll.remeasure();
        }
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.perform_message_action(action).await;
            this.update(cx, |this, cx| {
                if let Some(key) = retry_key {
                    this.retrying_messages.remove(&key);
                }
                if let Err(error) = result {
                    if let Some(starred) = desired_star
                        && let Some(row) = this
                            .messages
                            .rows
                            .iter_mut()
                            .find(|row| row.chat == target.chat && row.id == target.message)
                        && row.starred == starred
                    {
                        row.starred = !starred;
                    }
                    if let Some((previous, optimistic)) = &reaction_change
                        && let Some(row) = this
                            .messages
                            .rows
                            .iter_mut()
                            .find(|row| row.chat == target.chat && row.id == target.message)
                        && row.reactions == *optimistic
                    {
                        row.reactions = previous.clone();
                        this.messages.rebuild();
                        this.msg_scroll.remeasure();
                    }
                    this.send_error = Some(error.ui_message().to_string());
                }
                if this.chats.selected.as_deref() == Some(target.chat.as_str()) {
                    this.refresh_current_messages(cx);
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
        self.group_text_edit_error = None;
        cx.notify();
    }

    pub(crate) fn begin_group_text_edit(
        &mut self,
        field: GroupTextField,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.group_mutation_in_progress || self.group_leave_uncertain {
            return;
        }
        let input = match field {
            GroupTextField::Subject => &self.group_info_subject_input,
            GroupTextField::Description => &self.group_info_description_input,
        };
        input.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.focus(window, cx);
        });
        self.group_text_edit_error = None;
        self.message_overlay = Some(MessageOverlay::EditGroupText(field));
        cx.notify();
    }

    pub(crate) fn submit_group_text_edit(&mut self, cx: &mut Context<Self>) {
        let Some(MessageOverlay::EditGroupText(field)) = self.message_overlay.clone() else {
            return;
        };
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            self.close_message_overlay(cx);
            return;
        };
        let value = match field {
            GroupTextField::Subject => self.group_info_subject_input.read(cx).value().to_string(),
            GroupTextField::Description => self
                .group_info_description_input
                .read(cx)
                .value()
                .to_string(),
        };
        let patch = match field {
            GroupTextField::Subject => {
                wasabi_domain::GroupPatch::subject(details.chat.clone(), value)
            }
            GroupTextField::Description => {
                wasabi_domain::GroupPatch::description(details.chat.clone(), value)
            }
        };
        let patch = match patch {
            Ok(patch) => patch,
            Err(error) => {
                self.group_text_edit_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.message_overlay = None;
        self.group_text_edit_error = None;
        self.apply_group_patch(patch, cx);
    }

    pub(crate) fn open_group_member_actions(
        &mut self,
        target: GroupMemberTarget,
        cx: &mut Context<Self>,
    ) {
        if self.group_mutation_in_progress || !self.group_member_target_is_actionable(&target) {
            return;
        }
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.message_overlay = Some(MessageOverlay::GroupMemberActions(target));
        cx.notify();
    }

    pub(crate) fn confirm_group_member_action(
        &mut self,
        target: GroupMemberTarget,
        kind: GroupMemberActionKind,
        cx: &mut Context<Self>,
    ) {
        if !self.group_member_target_is_actionable(&target)
            || !group_member_action_matches_role(kind, target.participant_role)
        {
            self.message_overlay = None;
            self.group_mutation_error = Some(
                "This participant can no longer be managed from the current group state."
                    .to_string(),
            );
            cx.notify();
            return;
        }
        self.message_overlay = Some(MessageOverlay::ConfirmGroupMember(GroupMemberAction {
            target,
            kind,
        }));
        cx.notify();
    }

    pub(crate) fn run_confirmed_group_member_action(&mut self, cx: &mut Context<Self>) {
        let Some(MessageOverlay::ConfirmGroupMember(action)) = self.message_overlay.take() else {
            return;
        };
        if !self.group_member_target_is_actionable(&action.target)
            || !group_member_action_matches_role(action.kind, action.target.participant_role)
        {
            self.group_mutation_error = Some(
                "This participant can no longer be managed from the current group state."
                    .to_string(),
            );
            cx.notify();
            return;
        }
        let patch = match action.kind {
            GroupMemberActionKind::Promote => wasabi_domain::GroupPatch::promote_participant(
                action.target.chat,
                action.target.participant,
            ),
            GroupMemberActionKind::Demote => wasabi_domain::GroupPatch::demote_participant(
                action.target.chat,
                action.target.participant,
            ),
            GroupMemberActionKind::Remove => wasabi_domain::GroupPatch::remove_participant(
                action.target.chat,
                action.target.participant,
            ),
        };
        self.apply_group_patch(patch, cx);
    }

    fn group_member_target_is_actionable(&self, target: &GroupMemberTarget) -> bool {
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            return false;
        };
        details.chat == target.chat
            && self.chats.selected.as_deref() == Some(target.chat.as_str())
            && !self.group_leave_uncertain
            && details.permissions.can_manage_members()
            && details.participants.iter().any(|participant| {
                participant.jid == target.participant.as_str()
                    && participant.display_name == target.participant_name
                    && participant.role == target.participant_role
                    && !participant.is_self
                    && participant.role != wasabi_domain::ParticipantRole::SuperAdmin
            })
    }

    pub(crate) fn confirm_leave_group(&mut self, target: GroupLeaveTarget, cx: &mut Context<Self>) {
        if self.group_mutation_in_progress
            || self.group_leave_uncertain
            || !self.group_leave_target_is_current(&target)
        {
            return;
        }
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.message_overlay = Some(MessageOverlay::ConfirmLeaveGroup(target));
        cx.notify();
    }

    pub(crate) fn run_confirmed_leave_group(&mut self, cx: &mut Context<Self>) {
        let Some(MessageOverlay::ConfirmLeaveGroup(target)) = self.message_overlay.take() else {
            return;
        };
        if !self.group_leave_target_is_current(&target) {
            self.group_mutation_error = Some(
                "This group can no longer be left from the current conversation state.".to_string(),
            );
            cx.notify();
            return;
        }
        self.apply_group_patch(wasabi_domain::GroupPatch::leave(target.chat), cx);
    }

    fn group_leave_target_is_current(&self, target: &GroupLeaveTarget) -> bool {
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            return false;
        };
        details.chat == target.chat
            && details.subject == target.group_name
            && self.chats.selected.as_deref() == Some(target.chat.as_str())
            && details.permissions.current_user_role.is_some()
            && details
                .participants
                .iter()
                .any(|participant| participant.is_self)
    }

    pub(crate) fn confirm_join_request(
        &mut self,
        action: JoinRequestAction,
        cx: &mut Context<Self>,
    ) {
        if self.group_mutation_in_progress
            || self.group_leave_uncertain
            || !self.join_request_target_is_current(&action.target)
        {
            return;
        }
        self.group_mutation_error = None;
        self.group_mutation_feedback = None;
        self.message_overlay = Some(MessageOverlay::ConfirmJoinRequest(action));
        cx.notify();
    }

    pub(crate) fn run_confirmed_join_request(&mut self, cx: &mut Context<Self>) {
        let Some(MessageOverlay::ConfirmJoinRequest(action)) = self.message_overlay.take() else {
            return;
        };
        if !self.join_request_target_is_current(&action.target) {
            self.group_mutation_error = Some(
                "This join request can no longer be reviewed from the current group state."
                    .to_string(),
            );
            cx.notify();
            return;
        }
        let patch = match action.kind {
            JoinRequestActionKind::Approve => {
                wasabi_domain::GroupPatch::approve_membership_request(
                    action.target.chat,
                    action.target.participant,
                )
            }
            JoinRequestActionKind::Decline => wasabi_domain::GroupPatch::reject_membership_request(
                action.target.chat,
                action.target.participant,
            ),
        };
        self.apply_group_patch(patch, cx);
    }

    fn join_request_target_is_current(&self, target: &JoinRequestTarget) -> bool {
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            return false;
        };
        details.chat == target.chat
            && details.subject == target.group_name
            && self.chats.selected.as_deref() == Some(target.chat.as_str())
            && self.session.state.is_connected()
            && !self.group_leave_uncertain
            && details.permissions.can_manage_members()
            && self.membership_requests.iter().any(|request| {
                request.jid == target.participant && request.display_name == target.participant_name
            })
    }

    fn reset_membership_requests(&mut self) {
        self.membership_requests_gen.fetch_add(1, Ordering::AcqRel);
        self.membership_requests.clear();
        self.membership_requests_loading = false;
        self.membership_requests_error = None;
    }

    fn load_membership_requests(&mut self, cx: &mut Context<Self>) {
        let Some(ConversationDetails::Group(details)) = self.conversation_details.as_ref() else {
            self.reset_membership_requests();
            return;
        };
        if !details.permissions.can_manage_members() || !self.show_right_panel {
            self.reset_membership_requests();
            return;
        }
        if !self.session.state.is_connected() {
            self.reset_membership_requests();
            return;
        }
        let chat = details.chat.clone();
        let generation = self.membership_requests_gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.membership_requests.clear();
        self.membership_requests_loading = true;
        self.membership_requests_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.membership_requests(chat).await;
            this.update(cx, |this, cx| {
                if this.membership_requests_gen.load(Ordering::Acquire) != generation
                    || !this.show_right_panel
                {
                    return;
                }
                this.membership_requests_loading = false;
                match result {
                    Ok(requests) => {
                        this.membership_requests = requests;
                        this.membership_requests_error = None;
                    }
                    Err(error) => {
                        this.membership_requests.clear();
                        this.membership_requests_error = Some(error.ui_message().to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        });
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
        let emoji = if row
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == emoji && reaction.reacted_by_me)
        {
            String::new()
        } else {
            emoji
        };
        let action = wasabi_domain::MessageAction::React {
            target: row.into(),
            emoji,
        };
        self.message_overlay = None;
        self.perform_message_action(action, cx);
    }

    pub(crate) fn begin_reply(
        &mut self,
        message: wasabi_domain::MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.messages.rows.iter().any(|row| row.id == message) {
            return;
        }
        if self.active_draft.edit_target.is_some() {
            self.send_error = Some("Finish or cancel the current edit first".to_string());
            cx.notify();
            return;
        }
        self.active_draft.reply_to = Some(message);
        self.message_overlay = None;
        self.queue_draft_save(cx);
        self.composer_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn cancel_reply(&mut self, cx: &mut Context<Self>) {
        if self.active_draft.reply_to.take().is_some() {
            self.queue_draft_save(cx);
            cx.notify();
        }
    }

    pub(crate) fn begin_edit(
        &mut self,
        message: wasabi_domain::MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self
            .messages
            .rows
            .iter()
            .find(|row| row.id == message)
            .cloned()
        else {
            return;
        };
        if !row.can_edit_text_at(chrono::Utc::now().timestamp_millis()) {
            self.message_overlay = None;
            self.send_error = Some("This message can no longer be edited".to_string());
            cx.notify();
            return;
        }
        let current = self.composer_input.read(cx).value().trim().to_string();
        if !current.is_empty()
            || self.active_draft.reply_to.is_some()
            || self
                .chats
                .selected
                .as_ref()
                .is_some_and(|chat| self.staged_attachments.contains_key(chat))
        {
            self.message_overlay = None;
            self.send_error = Some("Finish or clear the current draft before editing".to_string());
            cx.notify();
            return;
        }
        let wasabi_domain::MessageKind::Text { body } = row.kind else {
            return;
        };
        self.active_draft.reply_to = None;
        self.active_draft.edit_target = Some(message);
        self.active_draft.body = body.clone();
        self.message_overlay = None;
        self.send_error = None;
        self.composer_input.update(cx, |input, cx| {
            composer::set_text_at_end(input, body, window, cx)
        });
        self.queue_draft_save(cx);
        self.composer_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.active_draft.edit_target.clone() else {
            return;
        };
        let Some(chat) = self.chats.selected.clone() else {
            return;
        };
        if self
            .editing_messages
            .contains(&(chat, target.as_str().to_string()))
        {
            return;
        }
        self.active_draft.edit_target = None;
        self.active_draft.body.clear();
        self.composer_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.queue_draft_save(cx);
        cx.notify();
    }

    fn submit_current_edit(
        &mut self,
        chat_id: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.active_draft.edit_target.clone() else {
            return;
        };
        let body = self.composer_input.read(cx).value().trim().to_string();
        if body.is_empty() {
            self.send_error = Some("Edited text cannot be empty".to_string());
            cx.notify();
            return;
        }
        let Some(row_index) = self
            .messages
            .rows
            .iter()
            .position(|row| row.chat.as_str() == chat_id && row.id == message)
        else {
            self.send_error = Some("Open the original message before editing it".to_string());
            cx.notify();
            return;
        };
        if !self.messages.rows[row_index].can_edit_text_at(chrono::Utc::now().timestamp_millis()) {
            self.send_error = Some("This message can no longer be edited".to_string());
            cx.notify();
            return;
        }
        let wasabi_domain::MessageKind::Text {
            body: previous_body,
        } = self.messages.rows[row_index].kind.clone()
        else {
            return;
        };
        if body == previous_body {
            self.cancel_edit(_window, cx);
            return;
        }
        let key = (chat_id.clone(), message.as_str().to_string());
        if !self.editing_messages.insert(key.clone()) {
            return;
        }
        let previous_edited_at = self.messages.rows[row_index].edited_at_ms;
        let optimistic_edited_at = chrono::Utc::now().timestamp_millis();
        self.messages.rows[row_index].kind =
            wasabi_domain::MessageKind::Text { body: body.clone() };
        self.messages.rows[row_index].edited_at_ms = Some(optimistic_edited_at);
        let action = wasabi_domain::MessageAction::Edit {
            target: (&self.messages.rows[row_index]).into(),
            body: body.clone(),
        };
        self.messages.rebuild();
        self.msg_scroll.remeasure();
        self.send_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.perform_message_action(action).await;
            if result.is_ok() {
                let _ = bridge
                    .save_draft(wasabi_domain::ChatId::new(chat_id.clone()), None)
                    .await;
            }
            let update = this.update(cx, |this, cx| {
                this.editing_messages.remove(&key);
                if let Err(error) = &result {
                    if let Some(row) = this.messages.rows.iter_mut().find(|row| {
                        row.chat.as_str() == chat_id && row.id == message
                    }) && row.edited_at_ms == Some(optimistic_edited_at)
                        && matches!(&row.kind, wasabi_domain::MessageKind::Text { body: current } if current == &body)
                    {
                        row.kind = wasabi_domain::MessageKind::Text {
                            body: previous_body.clone(),
                        };
                        row.edited_at_ms = previous_edited_at;
                        this.messages.rebuild();
                        this.msg_scroll.remeasure();
                    }
                    this.send_error = Some(error.ui_message().to_string());
                } else {
                    if let Some(summary) = this
                        .chats
                        .chats
                        .iter_mut()
                        .find(|summary| summary.id.as_str() == chat_id)
                        && submitted_edit_matches(summary.draft.as_ref(), &message, &body)
                    {
                        summary.draft = None;
                        summary.draft_preview = None;
                    }
                    let clear_visible_composer = should_clear_visible_edit(
                        this.chats.selected.as_deref(),
                        &chat_id,
                        this.active_draft.edit_target.as_ref(),
                        &message,
                        &this.composer_input.read(cx).value(),
                        &body,
                    );
                    if clear_visible_composer {
                        this.active_draft = wasabi_domain::Draft::default();
                    }
                    this.refresh_current_messages(cx);
                    this.refresh_visible();
                    cx.notify();
                    return (
                        this.window_handle,
                        this.composer_input.clone(),
                        clear_visible_composer,
                    );
                }
                this.refresh_visible();
                cx.notify();
                (this.window_handle, this.composer_input.clone(), false)
            });
            if result.is_ok()
                && let Ok((window_handle, input, true)) = update
            {
                window_handle
                    .update(cx, |_, window, cx| {
                        if input.read(cx).value().trim() == body {
                            input.update(cx, |state, cx| state.set_value("", window, cx));
                        }
                    })
                    .ok();
            }
        });
        cx.notify();
    }

    pub(crate) fn reveal_quoted_message(
        &mut self,
        message: wasabi_domain::MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.messages.timeline_index_for_message(&message) {
            self.messages.highlighted = Some(message);
            self.msg_scroll.scroll_to_reveal_item(index);
            cx.notify();
        } else if let Some(chat) = self.chats.selected.clone() {
            self.open_search_result(chat, message, window, cx);
        }
    }

    pub(crate) fn retry_message(
        &mut self,
        message: wasabi_domain::MessageId,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.messages.rows.iter().find(|row| row.id == message) else {
            return;
        };
        if row.direction != wasabi_domain::MessageDirection::Outgoing
            || row.status != wasabi_domain::MessageStatus::Failed
        {
            return;
        }
        self.perform_message_action(
            wasabi_domain::MessageAction::Retry { target: row.into() },
            cx,
        );
    }

    pub(crate) fn dismiss_overlay_or_drawer(&mut self, cx: &mut Context<Self>) {
        if self.new_chat_open {
            if !self.group_creating {
                self.close_new_chat(cx);
            }
        } else if self.settings_overlay.take().is_some() || self.message_overlay.take().is_some() {
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
        if action.is_destructive() {
            self.perform_destructive_chat_action(action, cx);
            return;
        }
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
                wasabi_domain::ChatAction::Clear { .. }
                | wasabi_domain::ChatAction::Delete { .. } => return,
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

    pub(crate) fn confirm_chat_action(
        &mut self,
        action: wasabi_domain::ChatAction,
        cx: &mut Context<Self>,
    ) {
        if action.is_destructive() {
            self.message_overlay = Some(MessageOverlay::ConfirmChat(action));
            cx.notify();
        }
    }

    pub(crate) fn run_confirmed_chat_action(&mut self, cx: &mut Context<Self>) {
        let Some(MessageOverlay::ConfirmChat(action)) = self.message_overlay.take() else {
            return;
        };
        self.perform_chat_action(action, cx);
    }

    fn perform_destructive_chat_action(
        &mut self,
        action: wasabi_domain::ChatAction,
        cx: &mut Context<Self>,
    ) {
        let chat_id = action.chat().as_str().to_string();
        if !self
            .chats
            .chats
            .iter()
            .any(|chat| chat.id.as_str() == chat_id)
            || !self.destructive_chats.insert(chat_id.clone())
        {
            return;
        }
        let clear = matches!(&action, wasabi_domain::ChatAction::Clear { .. });
        self.message_overlay = None;
        self.send_error = None;
        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let result = bridge.perform_chat_action(action).await;
            this.update(cx, |this, cx| {
                this.destructive_chats.remove(&chat_id);
                if let Err(error) = result {
                    this.send_error = Some(error.ui_message().to_string());
                    cx.notify();
                    return;
                }

                if clear {
                    if this.chats.selected.as_deref() == Some(chat_id.as_str()) {
                        this.messages_gen.fetch_add(1, Ordering::AcqRel);
                        this.messages.reset_for_chat(&chat_id);
                        this.msg_scroll.reset(0);
                        this.pending_new_messages = 0;
                    }
                    if let Some(chat) = this
                        .chats
                        .chats
                        .iter_mut()
                        .find(|chat| chat.id.as_str() == chat_id)
                    {
                        chat.last_message_preview = None;
                        chat.unread_count = 0;
                    }
                } else {
                    this.chats.chats.retain(|chat| chat.id.as_str() != chat_id);
                    if this.chats.selected.as_deref() == Some(chat_id.as_str()) {
                        this.stop_outbound_typing(chat_id.clone(), cx);
                        this.messages_gen.fetch_add(1, Ordering::AcqRel);
                        this.details_gen.fetch_add(1, Ordering::AcqRel);
                        this.chats.selected = None;
                        this.messages = MessageWindowModel::new();
                        this.msg_scroll.reset(0);
                        this.active_draft = wasabi_domain::Draft::default();
                        this.show_right_panel = false;
                        this.conversation_details = None;
                        this.details_loading = false;
                        this.details_error = None;
                        this.pending_new_messages = 0;
                    }
                }
                this.refresh_visible();
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
                        this.settings_feedback =
                            Some(SettingsFeedback::Error(error.ui_message().to_string()));
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
                        this.settings_feedback =
                            Some(SettingsFeedback::Error(error.ui_message().to_string()));
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
                        this.settings_feedback =
                            Some(SettingsFeedback::Success("Media cache cleared".to_string()));
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "media cache clear failed");
                        this.settings_feedback =
                            Some(SettingsFeedback::Error(error.ui_message().to_string()));
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
                        this.active_draft = wasabi_domain::Draft::default();
                        this.editing_messages.clear();
                        this.retrying_messages.clear();
                        this.destructive_chats.clear();
                        this.new_chat_open = false;
                        this.contacts.clear();
                        this.contacts_next = None;
                        this.contacts_loading = false;
                        this.contacts_error = None;
                        this.contacts_gen.fetch_add(1, Ordering::AcqRel);
                        this.phone_lookup = PhoneLookupUi::Idle;
                        this.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
                        this.new_chat_mode = NewChatMode::Direct;
                        this.group_participants.clear();
                        this.group_creation_error = None;
                        this.group_creating = false;
                        this.group_creation_uncertain = false;
                        this.group_creation_gen.fetch_add(1, Ordering::AcqRel);
                        this.typing.clear();
                        this.notification_seen.clear();
                        this.notification_seen_order.clear();
                    }
                    Err(error) => {
                        tracing::warn!(kind = %error.kind, "account logout failed");
                        this.settings_feedback =
                            Some(SettingsFeedback::Error(error.ui_message().to_string()));
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
        if before == after {
            // Stable message identities can still change rendered geometry
            // through an edit, revoke, reaction aggregate, or media state.
            // The bounded 200-row window makes an explicit native remeasure
            // cheap while keeping unchanged scroll identities intact.
            self.msg_scroll.remeasure();
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
                if this.chats_gen.load(Ordering::Acquire) != generation || this.chats.scope != scope
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
                            this.pending_new_messages = this.pending_new_messages.max(unseen);
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
                this.update_in(cx, |this, window, cx| this.select_chat(chat, window, cx))
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
                        Invalidation::Chats => this.refresh_chats(cx),
                        Invalidation::Contacts => {
                            this.refresh_chats(cx);
                            if this.new_chat_open {
                                this.load_contact_query(false, cx);
                            }
                            if let Some(selected) = this.chats.selected.clone()
                                && this
                                    .chats
                                    .chats
                                    .iter()
                                    .find(|chat| chat.id.as_str() == selected)
                                    .is_some_and(|chat| {
                                        matches!(chat.kind, ChatKind::Direct | ChatKind::Group)
                                    })
                            {
                                this.request_avatar(selected, true, cx);
                            }
                            if this.show_right_panel
                                && matches!(
                                    this.conversation_details.as_ref(),
                                    Some(
                                        ConversationDetails::Direct(_)
                                            | ConversationDetails::Group(_)
                                    )
                                )
                            {
                                this.load_conversation_details(cx);
                            }
                        }
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
        let generation = self.phone_pair_ticker_gen.fetch_add(1, Ordering::AcqRel) + 1;
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

        let mut root = div()
            .id("root")
            .size_full()
            .relative()
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
            .child(main_content(self, window, cx, pairing_active));
        if self.new_chat_open {
            root = root.child(if self.new_chat_mode == NewChatMode::Direct {
                new_chat::overlay(self, cx)
            } else {
                new_group::overlay(self, cx)
            });
        }
        root.into_any_element()
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
            item = item
                .bg(theme::row_selected())
                .text_color(theme::accent_text());
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
                        item.bg(theme::row_selected())
                            .text_color(theme::accent_text())
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
        draft: None,
    }
}

fn typing_refresh_due(last_sent: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last_sent.is_none_or(|last| now.duration_since(last) >= TYPING_REFRESH_AFTER)
}

fn should_restore_composer(accepted: bool, text_only: bool, current: &str) -> bool {
    !accepted && text_only && current.trim().is_empty()
}

fn optimistic_own_reaction(
    current: &[wasabi_domain::ReactionSummary],
    emoji: &str,
) -> Vec<wasabi_domain::ReactionSummary> {
    let mut next = current.to_vec();
    for reaction in &mut next {
        if reaction.reacted_by_me {
            reaction.reacted_by_me = false;
            reaction.count = reaction.count.saturating_sub(1);
        }
    }
    next.retain(|reaction| reaction.count > 0);
    if !emoji.is_empty() {
        if let Some(reaction) = next.iter_mut().find(|reaction| reaction.emoji == emoji) {
            reaction.count = reaction.count.saturating_add(1);
            reaction.reacted_by_me = true;
        } else {
            next.push(wasabi_domain::ReactionSummary {
                emoji: emoji.to_string(),
                count: 1,
                reacted_by_me: true,
            });
        }
    }
    next
}

fn submitted_edit_matches(
    draft: Option<&wasabi_domain::Draft>,
    target: &wasabi_domain::MessageId,
    body: &str,
) -> bool {
    draft.is_some_and(|draft| {
        draft.edit_target.as_ref() == Some(target) && draft.body.trim() == body
    })
}

fn should_clear_visible_edit(
    selected_chat: Option<&str>,
    submitted_chat: &str,
    active_target: Option<&wasabi_domain::MessageId>,
    submitted_target: &wasabi_domain::MessageId,
    current_body: &str,
    submitted_body: &str,
) -> bool {
    selected_chat == Some(submitted_chat)
        && active_target == Some(submitted_target)
        && current_body.trim() == submitted_body
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

fn normalized_contact_query(input: &str) -> String {
    wasabi_domain::ContactPhoneNumber::parse(input)
        .map(|phone| phone.as_str().to_string())
        .unwrap_or_else(|_| input.to_string())
}

fn group_creation_failure(kind: wasabi_domain::ErrorKind) -> (String, bool) {
    match kind {
        wasabi_domain::ErrorKind::RateLimited => (
            "Too many group requests. Wait a little, then try again.".to_string(),
            false,
        ),
        wasabi_domain::ErrorKind::Timeout => (
            "Group creation timed out. Check Chats before retrying.".to_string(),
            true,
        ),
        wasabi_domain::ErrorKind::NotConnected => (
            "Connection lost. Reconnect, then try again.".to_string(),
            false,
        ),
        _ => (
            "Couldn’t create this group. Review the members and try again.".to_string(),
            false,
        ),
    }
}

fn group_member_add_failure(kind: wasabi_domain::ErrorKind) -> (String, bool) {
    match kind {
        wasabi_domain::ErrorKind::RateLimited => (
            "Too many group requests. Wait a little, then try again.".to_string(),
            false,
        ),
        wasabi_domain::ErrorKind::Timeout => (
            "Adding members timed out. Refresh the group before retrying.".to_string(),
            true,
        ),
        wasabi_domain::ErrorKind::NotConnected => (
            "Connection lost. Reconnect, refresh the group, then try again.".to_string(),
            true,
        ),
        _ => (
            "Couldn’t add these members. Review the selection and try again.".to_string(),
            false,
        ),
    }
}

fn group_member_action_matches_role(
    kind: GroupMemberActionKind,
    role: wasabi_domain::ParticipantRole,
) -> bool {
    matches!(
        (kind, role),
        (
            GroupMemberActionKind::Promote,
            wasabi_domain::ParticipantRole::Member
        ) | (
            GroupMemberActionKind::Demote,
            wasabi_domain::ParticipantRole::Admin
        ) | (
            GroupMemberActionKind::Remove,
            wasabi_domain::ParticipantRole::Member
        ) | (
            GroupMemberActionKind::Remove,
            wasabi_domain::ParticipantRole::Admin
        )
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
            if matches!(this.phone_lookup, PhoneLookupUi::Checking) {
                this.phone_lookup_gen.fetch_add(1, Ordering::AcqRel);
                this.phone_lookup = PhoneLookupUi::Failed(
                    "Connection lost. Reconnect, then try again.".to_string(),
                );
            }
            if this.group_creating {
                this.group_creation_gen.fetch_add(1, Ordering::AcqRel);
                this.group_creating = false;
                this.group_creation_uncertain = true;
                this.group_creation_error = Some(
                    if this.new_chat_mode == NewChatMode::AddGroupMembers {
                        "Connection lost while adding members. Refresh the group after reconnecting before retrying."
                            .to_string()
                    } else {
                        "Connection lost while creating the group. Check Chats after reconnecting."
                            .to_string()
                    },
                );
            }
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
        if this.session.state.is_connected() && this.show_right_panel {
            this.load_membership_requests(cx);
        } else if !this.session.state.is_connected() {
            this.reset_membership_requests();
        }
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
    use super::{
        GroupMemberActionKind, TYPING_REFRESH_AFTER, TypingDisplay, group_creation_failure,
        group_member_action_matches_role, group_member_add_failure, normalized_contact_query,
        optimistic_own_reaction, should_clear_visible_edit, should_restore_composer,
        submitted_edit_matches, timeline_splice, typing_refresh_due,
    };

    #[test]
    fn formatted_phone_searches_use_canonical_digits_for_the_cache() {
        assert_eq!(normalized_contact_query("+1 (555) 123-4567"), "15551234567");
        assert_eq!(normalized_contact_query("Avery Chen"), "Avery Chen");
    }

    #[test]
    fn ambiguous_group_creation_failure_blocks_blind_retry() {
        let (_, timeout_uncertain) = group_creation_failure(wasabi_domain::ErrorKind::Timeout);
        let (_, offline_uncertain) = group_creation_failure(wasabi_domain::ErrorKind::NotConnected);
        let (_, rejected_uncertain) = group_creation_failure(wasabi_domain::ErrorKind::Protocol);
        assert!(timeout_uncertain);
        assert!(!offline_uncertain);
        assert!(!rejected_uncertain);
    }

    #[test]
    fn ambiguous_member_add_failure_requires_metadata_refresh() {
        let (_, timeout_uncertain) = group_member_add_failure(wasabi_domain::ErrorKind::Timeout);
        let (_, offline_uncertain) =
            group_member_add_failure(wasabi_domain::ErrorKind::NotConnected);
        let (_, rejected_uncertain) = group_member_add_failure(wasabi_domain::ErrorKind::Protocol);
        assert!(timeout_uncertain);
        assert!(offline_uncertain);
        assert!(!rejected_uncertain);
    }

    #[test]
    fn group_member_role_actions_never_target_creator_or_wrong_role() {
        use wasabi_domain::ParticipantRole;

        assert!(group_member_action_matches_role(
            GroupMemberActionKind::Promote,
            ParticipantRole::Member
        ));
        assert!(group_member_action_matches_role(
            GroupMemberActionKind::Demote,
            ParticipantRole::Admin
        ));
        assert!(!group_member_action_matches_role(
            GroupMemberActionKind::Promote,
            ParticipantRole::Admin
        ));
        assert!(!group_member_action_matches_role(
            GroupMemberActionKind::Remove,
            ParticipantRole::SuperAdmin
        ));
    }

    #[test]
    fn composing_updates_are_throttled_until_refresh_interval() {
        let now = std::time::Instant::now();
        assert!(typing_refresh_due(None, now));
        assert!(!typing_refresh_due(Some(now), now));
        assert!(typing_refresh_due(Some(now), now + TYPING_REFRESH_AFTER));
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
        assert_eq!(
            timeline_splice(&["date", "m2"], &["date", "m1", "m2"]),
            (1..1, 1)
        );
        assert_eq!(
            timeline_splice(&["date", "m1", "m2"], &["date", "m2", "m3"]),
            (1..3, 2)
        );
        assert_eq!(timeline_splice(&["date", "m1"], &["date", "m1"]), (2..2, 0));
    }

    #[test]
    fn only_pre_durable_text_failure_restores_an_untouched_composer() {
        assert!(should_restore_composer(false, true, ""));
        assert!(!should_restore_composer(true, true, ""));
        assert!(!should_restore_composer(false, false, ""));
        assert!(!should_restore_composer(false, true, "new draft"));
    }

    #[test]
    fn successful_edit_clears_only_the_exact_submitted_draft() {
        let target = wasabi_domain::MessageId::new("message-a");
        let exact = wasabi_domain::Draft {
            body: " corrected ".to_string(),
            edit_target: Some(target.clone()),
            ..Default::default()
        };
        assert!(submitted_edit_matches(Some(&exact), &target, "corrected"));

        let newer_text = wasabi_domain::Draft {
            body: "newer correction".to_string(),
            ..exact.clone()
        };
        assert!(!submitted_edit_matches(
            Some(&newer_text),
            &target,
            "corrected"
        ));
        assert!(!submitted_edit_matches(
            Some(&exact),
            &wasabi_domain::MessageId::new("message-b"),
            "corrected"
        ));
    }

    #[test]
    fn edit_completion_never_clears_an_identical_draft_after_chat_switch() {
        let target = wasabi_domain::MessageId::new("message-a");
        assert!(should_clear_visible_edit(
            Some("chat-a"),
            "chat-a",
            Some(&target),
            &target,
            "corrected",
            "corrected",
        ));
        assert!(!should_clear_visible_edit(
            Some("chat-b"),
            "chat-a",
            Some(&target),
            &target,
            "corrected",
            "corrected",
        ));
    }

    #[test]
    fn optimistic_reaction_replaces_or_removes_only_our_choice() {
        let existing = vec![
            wasabi_domain::ReactionSummary {
                emoji: "👍".to_string(),
                count: 2,
                reacted_by_me: true,
            },
            wasabi_domain::ReactionSummary {
                emoji: "❤️".to_string(),
                count: 3,
                reacted_by_me: false,
            },
        ];
        let changed = optimistic_own_reaction(&existing, "❤️");
        assert_eq!(changed[0].count, 1);
        assert!(!changed[0].reacted_by_me);
        assert_eq!(changed[1].count, 4);
        assert!(changed[1].reacted_by_me);

        let removed = optimistic_own_reaction(&changed, "");
        assert_eq!(removed[1].count, 3);
        assert!(!removed[1].reacted_by_me);
    }
}
