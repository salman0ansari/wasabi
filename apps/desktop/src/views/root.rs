//! Root window entity: three-pane shell layout, degraded states, and every
//! long-lived bridge task (hydration, invalidations, session/QR watches).
//!
//! All background work runs through [`Context::spawn`], which hands each task
//! a weak handle; every wake-up re-checks `upgrade()` before touching state,
//! and stale async results are dropped via per-view generation counters.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::prelude::*;
use gpui::{
    Context, FocusHandle, Focusable, Global, KeyBinding, Subscription, WeakEntity, Window, div, px,
};
use gpui_component::VirtualListScrollHandle;
use gpui_component::input::{InputEvent, InputState};
use gpui_component::{Icon, IconName};

use crate::core_bridge::CoreBridge;
use crate::state::chats::ChatFilter;
use crate::state::{ChatListModel, DeviceSettings, MessageWindowModel, SessionMirror, SettingsSection};
use crate::theme;
use crate::views::{chat_list, composer, conversation, pairing, right_panel, settings};

gpui::actions!(wasabi_desktop, [FocusSearch, OpenSettings, CloseInfo]);

pub const MAIN_KEY_CONTEXT: &str = "Main";
const CHAT_PAGE_LIMIT: usize = 100;
const MESSAGE_PAGE_LIMIT: usize = 60;
/// Countdown label refresh interval.
const COUNTDOWN_TICK: std::time::Duration = std::time::Duration::from_secs(1);

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
pub struct BridgeGlobal(pub Arc<CoreBridge>);

impl Global for BridgeGlobal {}

pub struct MainWindow {
    pub(crate) bridge: Arc<CoreBridge>,
    focus: FocusHandle,
    pub(crate) chats: ChatListModel,
    pub(crate) messages: MessageWindowModel,
    pub(crate) session: SessionMirror,
    pub(crate) typing: HashMap<String, ()>,
    nav_destination: NavDestination,
    pub(crate) show_right_panel: bool,
    pub(crate) settings: DeviceSettings,
    pub(crate) settings_section: SettingsSection,
    pub(crate) send_error: Option<String>,
    pub(crate) composer_input: gpui::Entity<InputState>,
    pub(crate) search_input: gpui::Entity<InputState>,
    pub(crate) chat_scroll: VirtualListScrollHandle,
    pub(crate) msg_scroll: VirtualListScrollHandle,
    /// First visible timeline index observed on the last frame.
    pub(crate) first_visible: usize,
    /// Whether the last frame showed the newest end of the timeline.
    pub(crate) near_bottom: bool,
    chats_gen: AtomicU64,
    messages_gen: AtomicU64,
    qr_ticker_gen: AtomicU64,
    pairing_request_gen: AtomicU64,
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

        let mut this = Self {
            bridge,
            focus: cx.focus_handle(),
            chats: ChatListModel::new(),
            messages: MessageWindowModel::new(),
            session: SessionMirror::new(),
            typing: HashMap::new(),
            nav_destination: NavDestination::Chats,
            show_right_panel: false,
            settings: DeviceSettings::load(),
            settings_section: SettingsSection::Chats,
            send_error: None,
            composer_input,
            search_input,
            chat_scroll: VirtualListScrollHandle::new(),
            msg_scroll: VirtualListScrollHandle::new(),
            first_visible: 0,
            near_bottom: true,
            chats_gen: AtomicU64::new(0),
            messages_gen: AtomicU64::new(0),
            qr_ticker_gen: AtomicU64::new(0),
            pairing_request_gen: AtomicU64::new(0),
            subscriptions: Vec::new(),
        };
        // Storage normally opens before the window appears; reflect a
        // not-yet-ready store as loading regardless of startup speed.
        this.chats.loading = !this.bridge.store_ready();

        let on_search_change = cx.subscribe_in(&this.search_input, window, {
            let search_input = this.search_input.clone();
            move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.chats.query = search_input.read(cx).value().to_string();
                    this.refresh_visible();
                    cx.notify();
                }
            }
        });
        this.subscriptions.push(on_search_change);

        // Deterministic teardown mirrors the supervisor sequence: flush
        // durable boundaries first, then stop the session. The callback is
        // sync at this rev; the async body parks on a detached task.
        let on_quit = cx.on_app_quit(|this, _cx| {
            let bridge = Arc::clone(&this.bridge);
            async move {
                let _ = bridge.flush_storage().await;
                let _ = bridge.stop_session().await;
            }
        });
        this.subscriptions.push(on_quit);

        this.spawn_hydration(cx);
        this.spawn_invalidation_loop(cx);
        this.spawn_state_watch(cx);
        this.spawn_qr_watch(cx);
        window.focus(&this.focus, cx);
        this
    }

    // ---- User intents ------------------------------------------------------

    pub(crate) fn set_chat_filter(&mut self, filter: ChatFilter, cx: &mut Context<Self>) {
        self.chats.filter = filter;
        self.refresh_visible();
        cx.notify();
    }

    fn select_nav(&mut self, destination: NavDestination, cx: &mut Context<Self>) {
        self.nav_destination = destination;
        if let Some(filter) = destination.chat_filter() {
            self.set_chat_filter(filter, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn select_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        if self.chats.selected.as_deref() == Some(chat_id.as_str()) {
            return;
        }
        // Bump first so any in-flight message load is discarded as stale.
        self.messages_gen.fetch_add(1, Ordering::AcqRel);
        self.chats.selected = Some(chat_id.clone());
        self.messages.reset_for_chat(&chat_id);
        self.show_right_panel = false;
        self.first_visible = 0;
        self.near_bottom = true;
        let generation = self.next_messages_gen();

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let page = bridge
                .load_message_page(&chat_id, None, MESSAGE_PAGE_LIMIT)
                .await;
            this.update(cx, |this, cx| {
                if this.messages_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match page {
                    Ok(page) => {
                        this.messages.anchor_newest(&page);
                        this.msg_scroll.scroll_to_bottom();
                    }
                    Err(err) => this.messages.set_error(err),
                }
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    pub(crate) fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat_id) = self.chats.selected.clone() else {
            return;
        };
        let text = self.composer_input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.send_error = None;
        // Clear immediately; the durable row arrives via invalidation.
        self.composer_input
            .update(cx, |state, cx| state.set_value("", window, cx));

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            if let Err(err) = bridge.send_text(chat_id, text).await {
                tracing::warn!(error = %err, "send failed");
                this.update(cx, |this, cx| {
                    this.send_error = Some(err);
                    cx.notify();
                })
                .ok();
            }
        });
        cx.notify();
    }

    pub(crate) fn request_pairing(&mut self, cx: &mut Context<Self>) {
        self.start_pairing_request(cx, false);
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
        self.show_right_panel = !self.show_right_panel;
        cx.notify();
    }

    pub(crate) fn close_right_panel(&mut self, cx: &mut Context<Self>) {
        self.show_right_panel = false;
        cx.notify();
    }

    pub(crate) fn select_settings_section(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        cx.notify();
    }

    pub(crate) fn save_settings(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.settings.save() {
            self.send_error = Some(format!("Could not save settings: {error}"));
        }
        cx.notify();
    }

    pub(crate) fn refresh_visible(&mut self) {
        self.chats.visible_cache = self.chats.visible();
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
                        let items_before = this.messages.items.len();
                        let added_rows = this.messages.prepend_older(&page);
                        let inserted_items = this.messages.items.len().saturating_sub(items_before);
                        if added_rows > 0 && inserted_items > 0 {
                            // Anchor on the first newly inserted item so the
                            // reading position stays put across the prepend.
                            this.msg_scroll
                                .scroll_to_item(inserted_items, gpui::ScrollStrategy::Top);
                        }
                    }
                    Err(err) => this.messages.set_error(err),
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

        let bridge = Arc::clone(&self.bridge);
        spawn_main(cx, async move |this, cx| {
            let page = bridge.load_chat_page(false, None, CHAT_PAGE_LIMIT).await;
            let has_more = page
                .as_ref()
                .is_ok_and(|rows| rows.len() == CHAT_PAGE_LIMIT);
            this.update(cx, |this, cx| {
                if this.chats_gen.load(Ordering::Acquire) != generation {
                    return;
                }
                match page {
                    Ok(rows) => {
                        this.chats.set_page(rows, has_more);
                        this.refresh_visible();
                    }
                    Err(err) => this.chats.set_error(err),
                }
                cx.notify();
            })
            .ok();
        });
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
                        if this.near_bottom || this.messages.rows.is_empty() {
                            this.messages.anchor_newest(page);
                            this.msg_scroll.scroll_to_bottom();
                        } else {
                            // Mid-history: fold newer rows in place.
                            this.messages.merge_newer(page);
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
            let page = bridge.load_chat_page(false, None, CHAT_PAGE_LIMIT).await;
            let has_more = page
                .as_ref()
                .is_ok_and(|rows| rows.len() == CHAT_PAGE_LIMIT);
            let first_chat = page
                .as_ref()
                .ok()
                .and_then(|rows| rows.first().map(|c| c.id.as_str().to_string()));

            this.update(cx, |this, cx| {
                match &page {
                    Ok(rows) => {
                        this.chats.set_page(rows.clone(), has_more);
                        this.refresh_visible();
                    }
                    Err(err) => this.chats.set_error(err.clone()),
                }
                cx.notify();
            })
            .ok();

            if let Some(chat) = first_chat {
                this.update(cx, |this, cx| this.select_chat(chat, cx)).ok();
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
                            ) {
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

    // ---- Generation helpers ------------------------------------------------

    fn next_chats_gen(&self) -> u64 {
        self.chats_gen.fetch_add(1, Ordering::AcqRel) + 1
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
                this.close_right_panel(cx);
            }))
            .child(main_content(self, window, cx, pairing_active))
            .into_any_element()
    }
}

fn nav_rail(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let items = [(IconName::Inbox, NavDestination::Chats)];

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

    for (index, (icon, destination)) in items.into_iter().enumerate() {
        let active = this.nav_destination == destination;
        let mut item = div()
            .id(("nav-item", index))
            .size(px(40.0))
            .rounded(px(theme::RADIUS_LG))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(18.0));
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
                    .text_color(theme::text_secondary())
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
        .child(pairing::pairing_panel(&this.session, cx))
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
        return pairing::pairing_panel(&this.session, cx).into_any_element();
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

// ---- Watch appliers --------------------------------------------------------

/// Apply one session-state update plus derived side effects.
fn apply_state(
    weak: WeakEntity<MainWindow>,
    cx: &mut gpui::AsyncApp,
    state: wasabi_core::state::SessionState,
) -> Result<(), anyhow::Error> {
    let connected = state.is_connected();
    weak.update(cx, |this, cx| {
        if state.is_connected() {
            this.session.connected_once = true;
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
        if connected && this.bridge.has_pending_sends() {
            let bridge = Arc::clone(&this.bridge);
            spawn_main(cx, async move |_, _| {
                bridge.flush_pending().await;
            });
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
