//! On-demand direct-contact and group information. Direct conversations never
//! render a participants section; groups in common come from the local cache
//! only. Group metadata remains honest until the backend projection has
//! populated real participants.

use gpui::prelude::*;
use gpui::{Context, px};
use gpui_component::{Icon, IconName};
use wasabi_domain::{ConversationDetails, Participant, ParticipantRole};

use crate::state::chats;
use crate::theme;
use crate::views::avatar;
use crate::views::root::MainWindow;

pub fn info_panel(this: &mut MainWindow, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let selected = this
        .chats
        .selected
        .as_ref()
        .and_then(|id| this.chats.chats.iter().find(|chat| chat.id.as_str() == id));

    let (fallback_name, is_group) = selected
        .map(|chat| {
            (
                chats::fallback_name(chat),
                chats::is_group(chat.id.as_str()),
            )
        })
        .unwrap_or_else(|| ("Conversation".to_string(), false));
    let (name, subtitle, about) = match this.conversation_details.as_ref() {
        Some(ConversationDetails::Direct(details)) => (
            details.display_name.clone(),
            details
                .phone_number
                .clone()
                .unwrap_or_else(|| "Contact".to_string()),
            details.about.clone(),
        ),
        Some(ConversationDetails::Group(details)) => (
            details.subject.clone(),
            format!("{} participants", details.participant_count),
            details.description.clone(),
        ),
        None => (
            fallback_name,
            if is_group { "Group" } else { "Contact" }.to_string(),
            None,
        ),
    };
    let initial = avatar::first_initial(&name);
    let selected_chat = this.chats.selected.clone();
    let photo = selected_chat.as_deref().and_then(|id| this.avatar_path(id));

    let mut panel = gpui::div()
        .id("conversation-info")
        .w(px(theme::RIGHT_PANEL_W))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .bg(theme::surface())
        .border_l_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .h(px(58.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .border_b_1()
                .border_color(theme::border())
                .child(
                    gpui::div()
                        .id("close-info")
                        .size(px(34.0))
                        .rounded_full()
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|style| style.bg(theme::row_hover()))
                        .text_color(theme::text_secondary())
                        .on_click(cx.listener(|this, _, _, cx| this.close_right_panel(cx)))
                        .child(Icon::new(IconName::ChevronLeft).size(px(18.0))),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_NAME))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(if is_group {
                            "Group info"
                        } else {
                            "Contact info"
                        }),
                ),
        )
        .child(
            gpui::div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(7.0))
                .px(px(20.0))
                .py(px(22.0))
                .child(avatar::avatar_face(
                    82.0,
                    photo,
                    initial,
                    theme::row_selected(),
                    theme::accent_text(),
                    Some(30.0),
                ))
                .child(
                    gpui::div()
                        .text_size(px(18.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::text_primary())
                        .child(name),
                )
                .child(
                    gpui::div()
                        .text_size(px(theme::TEXT_SIZE_SM))
                        .text_color(theme::text_secondary())
                        .child(subtitle),
                ),
        )
        .child(section(
            "ABOUT",
            about.unwrap_or_else(|| {
                if this.details_loading {
                    "Loading conversation information…".to_string()
                } else if is_group {
                    "No group description".to_string()
                } else {
                    "About unavailable".to_string()
                }
            }),
        ))
        .when(!is_group && !this.groups_in_common.is_empty(), |panel| {
            panel.child(groups_in_common_section(this, cx))
        })
        .child(chat_sync_action(this, cx, ChatSyncAction::Pin))
        .child(chat_sync_action(this, cx, ChatSyncAction::Mute))
        .child(chat_sync_action(this, cx, ChatSyncAction::Archive))
        .child(chat_sync_action(this, cx, ChatSyncAction::MarkRead))
        .child(favorite_action(this, cx))
        .child(action_row("Encryption", "End-to-end encrypted"));

    if let Some(error) = this.details_error.clone() {
        panel = panel.child(section("INFORMATION UNAVAILABLE", error));
    }

    if is_group {
        if let Some(ConversationDetails::Group(details)) = this.conversation_details.clone() {
            panel = panel.child(group_text_actions(this, cx, &details));
            panel = panel.child(group_permissions(this, cx, &details));
            if details.permissions.can_manage_members() {
                panel = panel.child(invite_link_section(this, cx, &details));
                panel = panel.child(join_requests_section(this, cx, &details));
            }
        }
        if let Some(error) = this.group_mutation_error.clone() {
            panel = panel.child(section("GROUP UPDATE FAILED", error));
        }
        if let Some(feedback) = this.group_mutation_feedback.clone() {
            panel = panel.child(section("GROUP UPDATED", feedback));
        }
        panel = panel.child(participants_section(this, cx));
        if let Some(ConversationDetails::Group(details)) = this.conversation_details.clone()
            && details.permissions.current_user_role.is_some()
            && details
                .participants
                .iter()
                .any(|participant| participant.is_self)
        {
            panel = panel.child(leave_group_action(this, cx, details));
        }
    } else {
        if let Some(error) = this.contact_mutation_error.clone() {
            panel = panel.child(section("CONTACT UPDATE FAILED", error));
        }
        if let Some(row) = contact_block_action(this, cx) {
            panel = panel.child(row);
        }
        if let Some(row) = contact_remove_action(this, cx) {
            panel = panel.child(row);
        }
    }

    panel = panel
        .child(destructive_chat_action(
            this,
            cx,
            DestructiveChatAction::Clear,
        ))
        .child(destructive_chat_action(
            this,
            cx,
            DestructiveChatAction::Delete,
        ));

    panel
}

fn leave_group_action(
    this: &MainWindow,
    cx: &mut Context<MainWindow>,
    details: wasabi_domain::GroupDetails,
) -> gpui::Stateful<gpui::Div> {
    let pending = this.group_mutation_in_progress;
    let blocked = pending || this.group_leave_uncertain;
    let target = crate::views::root::GroupLeaveTarget {
        chat: details.chat,
        group_name: details.subject,
    };
    gpui::div()
        .id("leave-group")
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .border_t_1()
        .border_color(theme::border())
        .when(!blocked, |row| {
            row.cursor_pointer()
                .aria_label("Leave group")
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.confirm_leave_group(target.clone(), cx)),
                )
        })
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(if blocked {
                    theme::text_secondary()
                } else {
                    theme::danger()
                })
                .child(if pending {
                    "Working…"
                } else if this.group_leave_uncertain {
                    "Reopen group info to confirm leave status"
                } else {
                    "Leave group"
                }),
        )
}

fn group_text_actions(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    details: &wasabi_domain::GroupDetails,
) -> gpui::Div {
    gpui::div()
        .child(group_text_action(
            this,
            cx,
            "edit-group-name",
            "Group name",
            details.subject.clone(),
            crate::views::root::GroupTextField::Subject,
            details,
        ))
        .child(group_text_action(
            this,
            cx,
            "edit-group-description",
            "Description",
            details
                .description
                .clone()
                .unwrap_or_else(|| "Not set".to_string()),
            crate::views::root::GroupTextField::Description,
            details,
        ))
}

fn group_text_action(
    this: &MainWindow,
    cx: &mut Context<MainWindow>,
    id: &'static str,
    label: &'static str,
    value: String,
    field: crate::views::root::GroupTextField,
    details: &wasabi_domain::GroupDetails,
) -> gpui::Stateful<gpui::Div> {
    let allowed = details.permissions.can_manage_members();
    let pending = this.group_mutation_in_progress;
    let blocked = pending || this.group_leave_uncertain;
    let edit_value = match field {
        crate::views::root::GroupTextField::Subject => details.subject.clone(),
        crate::views::root::GroupTextField::Description => {
            details.description.clone().unwrap_or_default()
        }
    };
    gpui::div()
        .id(id)
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .border_t_1()
        .border_color(theme::border())
        .when(allowed && !blocked, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin_group_text_edit(field, edit_value.clone(), window, cx)
                }))
        })
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(if allowed && !blocked {
                    theme::text_primary()
                } else {
                    theme::text_secondary()
                })
                .child(label),
        )
        .child(
            gpui::div()
                .max_w(px(190.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(if this.group_leave_uncertain {
                    "Refresh required".to_string()
                } else if allowed {
                    value
                } else {
                    format!("{value} · Admin required")
                }),
        )
}

#[derive(Clone, Copy)]
enum GroupPermissionAction {
    EditInfo,
    SendMessages,
    ApproveMembers,
}

fn group_permissions(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    details: &wasabi_domain::GroupDetails,
) -> gpui::Div {
    gpui::div()
        .child(group_permission_row(
            this,
            cx,
            details,
            GroupPermissionAction::EditInfo,
        ))
        .child(group_permission_row(
            this,
            cx,
            details,
            GroupPermissionAction::SendMessages,
        ))
        .child(group_permission_row(
            this,
            cx,
            details,
            GroupPermissionAction::ApproveMembers,
        ))
}

fn group_permission_row(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    details: &wasabi_domain::GroupDetails,
    action: GroupPermissionAction,
) -> gpui::Stateful<gpui::Div> {
    let allowed = details.permissions.can_manage_members();
    let pending = this.group_mutation_in_progress;
    let blocked = pending || this.group_leave_uncertain;
    let chat = details.chat.clone();
    let (id, label, enabled, patch) = match action {
        GroupPermissionAction::EditInfo => (
            "group-only-admins-edit",
            "Edit group info",
            details.permissions.only_admins_edit,
            wasabi_domain::GroupPatch::only_admins_edit(
                chat,
                !details.permissions.only_admins_edit,
            ),
        ),
        GroupPermissionAction::SendMessages => (
            "group-only-admins-send",
            "Send messages",
            details.permissions.only_admins_send,
            wasabi_domain::GroupPatch::only_admins_send(
                chat,
                !details.permissions.only_admins_send,
            ),
        ),
        GroupPermissionAction::ApproveMembers => (
            "group-membership-approval",
            "Approve new members",
            details.permissions.membership_approval,
            wasabi_domain::GroupPatch::membership_approval(
                chat,
                !details.permissions.membership_approval,
            ),
        ),
    };
    let value = match action {
        GroupPermissionAction::EditInfo | GroupPermissionAction::SendMessages => {
            if enabled {
                "Admins only"
            } else {
                "All participants"
            }
        }
        GroupPermissionAction::ApproveMembers => {
            if enabled {
                "On"
            } else {
                "Off"
            }
        }
    };
    let detail = if this.group_leave_uncertain {
        "Refresh required".to_string()
    } else if pending {
        "Saving…".to_string()
    } else if allowed {
        value.to_string()
    } else {
        format!("{value} · Admin required")
    };

    gpui::div()
        .id(id)
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .border_t_1()
        .border_color(theme::border())
        .when(allowed && !blocked, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.apply_group_patch(patch.clone(), cx)),
                )
        })
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(if allowed {
                    theme::text_primary()
                } else {
                    theme::text_secondary()
                })
                .child(label),
        )
        .child(
            gpui::div()
                .max_w(px(190.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(if enabled && allowed {
                    theme::accent_text()
                } else {
                    theme::text_secondary()
                })
                .child(detail),
        )
}

fn invite_link_section(
    this: &MainWindow,
    cx: &mut Context<MainWindow>,
    details: &wasabi_domain::GroupDetails,
) -> gpui::Div {
    let mut body = gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("INVITE LINK"),
        );
    if !this.session.state.is_connected() {
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child("Connect to load the invite link"),
        );
    }
    if this.invite_link_loading {
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child("Loading invite link…"),
        );
    }
    if this.invite_link.as_ref().is_none_or(|url| url.is_empty()) {
        let message = this
            .invite_link_error
            .clone()
            .unwrap_or_else(|| "Invite link is unavailable".to_string());
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(message),
        );
    }
    let blocked =
        this.invite_link_resetting || this.group_mutation_in_progress || this.group_leave_uncertain;
    body = body.child(invite_link_row(this, details, blocked, cx));
    if let Some(error) = this.invite_link_error.clone() {
        body = body.child(
            gpui::div()
                .pt(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(error),
        );
    }
    body
}

fn invite_link_row(
    this: &MainWindow,
    details: &wasabi_domain::GroupDetails,
    blocked: bool,
    cx: &mut Context<MainWindow>,
) -> gpui::Div {
    let url = this.invite_link.clone().unwrap_or_default();
    let reset_target = crate::views::root::InviteLinkResetTarget {
        chat: details.chat.clone(),
        group_name: details.subject.clone(),
    };
    let reset_label = if this.invite_link_resetting {
        "Resetting…"
    } else {
        "Reset…"
    };
    gpui::div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(
            gpui::div()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(url),
        )
        .child(
            gpui::div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(invite_link_action_button(
                    "copy-invite-link",
                    "Copy",
                    false,
                    false,
                    None,
                    cx,
                ))
                .child(invite_link_action_button(
                    "reset-invite-link",
                    reset_label,
                    true,
                    blocked,
                    Some(reset_target),
                    cx,
                )),
        )
}

fn invite_link_action_button(
    id: &'static str,
    label: &'static str,
    danger: bool,
    blocked: bool,
    reset: Option<crate::views::root::InviteLinkResetTarget>,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(4.0))
        .text_size(px(theme::TEXT_SIZE_SM))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if blocked {
            theme::text_secondary()
        } else if danger {
            theme::danger()
        } else {
            theme::accent_text()
        })
        .when(!blocked, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(target) = reset.clone() {
                        this.confirm_reset_invite_link(target, cx);
                    } else {
                        this.copy_invite_link(cx);
                    }
                }))
        })
        .child(label)
}

fn join_requests_section(
    this: &MainWindow,
    cx: &mut Context<MainWindow>,
    details: &wasabi_domain::GroupDetails,
) -> gpui::Div {
    let mut body = gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("JOIN REQUESTS"),
        );
    if !this.session.state.is_connected() {
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child("Connect to review join requests"),
        );
    }
    if this.membership_requests_loading {
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child("Loading join requests…"),
        );
    }
    if let Some(error) = this.membership_requests_error.clone() {
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(error),
        );
    }
    if this.membership_requests.is_empty() {
        return body.child(
            gpui::div()
                .py(px(8.0))
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child("No pending join requests"),
        );
    }
    let blocked = this.group_mutation_in_progress || this.group_leave_uncertain;
    for (index, request) in this.membership_requests.iter().enumerate() {
        body = body.child(join_request_row(request, details, index, blocked, cx));
    }
    body
}

fn join_request_row(
    request: &wasabi_domain::PendingMembershipRequest,
    details: &wasabi_domain::GroupDetails,
    index: usize,
    blocked: bool,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let approve = crate::views::root::JoinRequestAction {
        target: crate::views::root::JoinRequestTarget {
            chat: details.chat.clone(),
            group_name: details.subject.clone(),
            participant: request.jid.clone(),
            participant_name: request.display_name.clone(),
        },
        kind: crate::views::root::JoinRequestActionKind::Approve,
    };
    let decline = crate::views::root::JoinRequestAction {
        target: approve.target.clone(),
        kind: crate::views::root::JoinRequestActionKind::Decline,
    };
    gpui::div()
        .id(("join-request", index))
        .min_h(px(52.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(request.display_name.clone()),
        )
        .child(join_request_action_button(
            ("approve-join-request", index),
            "Approve",
            false,
            blocked,
            approve,
            cx,
        ))
        .child(join_request_action_button(
            ("decline-join-request", index),
            "Decline",
            true,
            blocked,
            decline,
            cx,
        ))
}

fn join_request_action_button(
    id: (&'static str, usize),
    label: &'static str,
    danger: bool,
    blocked: bool,
    action: crate::views::root::JoinRequestAction,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(4.0))
        .text_size(px(theme::TEXT_SIZE_SM))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if blocked {
            theme::text_secondary()
        } else if danger {
            theme::danger()
        } else {
            theme::accent_text()
        })
        .when(!blocked, |button| {
            button
                .cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.confirm_join_request(action.clone(), cx)
                    }),
                )
        })
        .child(label)
}

fn groups_in_common_section(this: &MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let mut body = gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("GROUPS IN COMMON"),
        );
    for (index, group) in this.groups_in_common.iter().enumerate() {
        body = body.child(groups_in_common_row(group, index, cx));
    }
    body
}

fn groups_in_common_row(
    group: &wasabi_domain::SharedGroup,
    index: usize,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let chat = group.chat.as_str().to_string();
    let subject = group.subject.clone();
    gpui::div()
        .id(("groups-in-common", index))
        .min_h(px(48.0))
        .flex()
        .items_center()
        .cursor_pointer()
        .aria_label(format!("Open {subject}"))
        .hover(|style| style.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.select_chat(chat.clone(), window, cx);
        }))
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(subject),
        )
        .child(
            Icon::new(IconName::ChevronRight)
                .size(px(15.0))
                .text_color(theme::text_secondary()),
        )
}

fn section(label: &'static str, body: impl Into<String>) -> gpui::Div {
    gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(6.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child(label),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_secondary())
                .child(body.into()),
        )
}

fn action_row(label: &'static str, detail: impl Into<String>) -> gpui::Div {
    gpui::div()
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(label),
        )
        .child(
            gpui::div()
                .max_w(px(170.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::text_secondary())
                .child(detail.into()),
        )
}

fn favorite_action(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let favorite = this
        .chats
        .selected
        .as_ref()
        .and_then(|selected| {
            this.chats
                .chats
                .iter()
                .find(|chat| chat.id.as_str() == selected)
        })
        .is_some_and(|chat| chat.favorite);
    gpui::div()
        .id("toggle-favorite")
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .cursor_pointer()
        .border_t_1()
        .border_color(theme::border())
        .hover(|style| style.bg(theme::row_hover()))
        .on_click(cx.listener(|this, _, _, cx| this.toggle_favorite(cx)))
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child("Favorite"),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(if favorite {
                    theme::accent_text()
                } else {
                    theme::text_secondary()
                })
                .child(if favorite { "On this device" } else { "Off" }),
        )
}

#[derive(Clone, Copy)]
enum ChatSyncAction {
    Pin,
    Mute,
    Archive,
    MarkRead,
}

fn chat_sync_action(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    kind: ChatSyncAction,
) -> gpui::Stateful<gpui::Div> {
    let selected = this.chats.selected.clone().unwrap_or_default();
    let summary = this
        .chats
        .chats
        .iter()
        .find(|chat| chat.id.as_str() == selected);
    let (label, enabled, detail, action) = match kind {
        ChatSyncAction::Pin => {
            let enabled = summary.is_some_and(|chat| chat.pinned_at_ms.is_some());
            (
                "Pin chat",
                enabled,
                if enabled { "On" } else { "Off" },
                wasabi_domain::ChatAction::Pin {
                    chat: wasabi_domain::ChatId::new(selected),
                    pinned: !enabled,
                },
            )
        }
        ChatSyncAction::Mute => {
            let now = chrono::Utc::now().timestamp_millis();
            let enabled = summary.is_some_and(|chat| {
                chat.muted_until_ms
                    .is_some_and(|until| until == 0 || until > now)
            });
            (
                "Mute notifications",
                enabled,
                if enabled { "On" } else { "Off" },
                wasabi_domain::ChatAction::Mute {
                    chat: wasabi_domain::ChatId::new(selected),
                    muted: !enabled,
                },
            )
        }
        ChatSyncAction::Archive => {
            let enabled = summary.is_some_and(|chat| chat.archived);
            (
                if enabled {
                    "Unarchive chat"
                } else {
                    "Archive chat"
                },
                enabled,
                if enabled { "Archived" } else { "Active" },
                wasabi_domain::ChatAction::Archive {
                    chat: wasabi_domain::ChatId::new(selected),
                    archived: !enabled,
                },
            )
        }
        ChatSyncAction::MarkRead => {
            let enabled = summary.is_some_and(|chat| chat.unread_count != 0);
            (
                if enabled {
                    "Mark as read"
                } else {
                    "Mark as unread"
                },
                enabled,
                if enabled { "Unread" } else { "Read" },
                wasabi_domain::ChatAction::MarkRead {
                    chat: wasabi_domain::ChatId::new(selected),
                    read: enabled,
                },
            )
        }
    };
    gpui::div()
        .id(match kind {
            ChatSyncAction::Pin => "toggle-pin",
            ChatSyncAction::Mute => "toggle-mute",
            ChatSyncAction::Archive => "toggle-archive",
            ChatSyncAction::MarkRead => "toggle-read",
        })
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .cursor_pointer()
        .border_t_1()
        .border_color(theme::border())
        .hover(|style| style.bg(theme::row_hover()))
        .on_click(cx.listener(move |this, _, _, cx| this.perform_chat_action(action.clone(), cx)))
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(label),
        )
        .child(
            gpui::div()
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(if enabled {
                    theme::accent_text()
                } else {
                    theme::text_secondary()
                })
                .child(detail),
        )
}

fn contact_block_action(
    this: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> Option<gpui::Stateful<gpui::Div>> {
    let ConversationDetails::Direct(details) = this.conversation_details.as_ref()? else {
        return None;
    };
    let is_blocked = details.is_blocked?;
    let pending = this.contact_mutation_in_progress;
    let connected = this.session.state.is_connected();
    let enabled = connected && !pending;
    let jid = wasabi_domain::ChatId::new(details.jid.clone());
    let action = if is_blocked {
        wasabi_domain::ContactAction::Unblock { jid }
    } else {
        wasabi_domain::ContactAction::Block { jid }
    };
    let (id, label) = if pending {
        (
            if is_blocked {
                "unblock-contact"
            } else {
                "block-contact"
            },
            "Working…",
        )
    } else if is_blocked {
        ("unblock-contact", "Unblock…")
    } else {
        ("block-contact", "Block…")
    };
    Some(
        gpui::div()
            .id(id)
            .mx(px(16.0))
            .min_h(px(52.0))
            .py(px(10.0))
            .flex()
            .items_center()
            .border_t_1()
            .border_color(theme::border())
            .when(enabled, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_contact_action(action.clone(), cx)
                    }))
            })
            .child(
                gpui::div()
                    .flex_1()
                    .text_size(px(theme::TEXT_SIZE))
                    .text_color(if enabled {
                        if is_blocked {
                            theme::text_primary()
                        } else {
                            theme::danger()
                        }
                    } else {
                        theme::text_secondary()
                    })
                    .child(label),
            ),
    )
}

fn contact_remove_action(
    this: &MainWindow,
    cx: &mut Context<MainWindow>,
) -> Option<gpui::Stateful<gpui::Div>> {
    let ConversationDetails::Direct(details) = this.conversation_details.as_ref()? else {
        return None;
    };
    let jid = bare_phone_jid(details)?;
    let pending = this.contact_mutation_in_progress;
    let connected = this.session.state.is_connected();
    let enabled = connected && !pending;
    let action = wasabi_domain::ContactAction::Remove { jid };
    Some(
        gpui::div()
            .id("delete-contact")
            .mx(px(16.0))
            .min_h(px(52.0))
            .py(px(10.0))
            .flex()
            .items_center()
            .border_t_1()
            .border_color(theme::border())
            .when(enabled, |row| {
                row.cursor_pointer()
                    .hover(|style| style.bg(theme::row_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.confirm_contact_action(action.clone(), cx)
                    }))
            })
            .child(
                gpui::div()
                    .flex_1()
                    .text_size(px(theme::TEXT_SIZE))
                    .text_color(if enabled {
                        theme::danger()
                    } else {
                        theme::text_secondary()
                    })
                    .child(if pending {
                        "Working…"
                    } else {
                        "Delete contact…"
                    }),
            ),
    )
}

fn bare_phone_jid(details: &wasabi_domain::DirectContactDetails) -> Option<wasabi_domain::ChatId> {
    let phone = details.phone_number.as_deref()?;
    let (user, server) = details.jid.split_once('@')?;
    if server != "s.whatsapp.net" || user.contains(':') || user != phone {
        return None;
    }
    Some(wasabi_domain::ChatId::new(details.jid.clone()))
}

#[derive(Clone, Copy)]
enum DestructiveChatAction {
    Clear,
    Delete,
}

fn destructive_chat_action(
    this: &mut MainWindow,
    cx: &mut Context<MainWindow>,
    kind: DestructiveChatAction,
) -> gpui::Stateful<gpui::Div> {
    let selected = this.chats.selected.clone().unwrap_or_default();
    let pending = this.destructive_chats.contains(&selected);
    let (id, label, action) = match kind {
        DestructiveChatAction::Clear => (
            "clear-chat",
            "Clear messages…",
            wasabi_domain::ChatAction::Clear {
                chat: wasabi_domain::ChatId::new(selected),
                delete_starred: false,
                delete_media: false,
            },
        ),
        DestructiveChatAction::Delete => (
            "delete-chat",
            "Delete chat…",
            wasabi_domain::ChatAction::Delete {
                chat: wasabi_domain::ChatId::new(selected),
                delete_media: false,
            },
        ),
    };
    gpui::div()
        .id(id)
        .mx(px(16.0))
        .min_h(px(52.0))
        .py(px(10.0))
        .flex()
        .items_center()
        .border_t_1()
        .border_color(theme::border())
        .when(!pending, |row| {
            row.cursor_pointer()
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.confirm_chat_action(action.clone(), cx)),
                )
        })
        .child(
            gpui::div()
                .flex_1()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(if pending {
                    theme::text_secondary()
                } else {
                    theme::danger()
                })
                .child(if pending { "Working…" } else { label }),
        )
}

fn participants_section(this: &MainWindow, cx: &mut Context<MainWindow>) -> gpui::Div {
    let mut body = gpui::div()
        .mx(px(16.0))
        .py(px(14.0))
        .border_t_1()
        .border_color(theme::border())
        .child(
            gpui::div()
                .mb(px(8.0))
                .text_size(px(theme::TEXT_SIZE_SM))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::accent_text())
                .child("PARTICIPANTS"),
        );

    match this.conversation_details.as_ref() {
        Some(ConversationDetails::Group(details)) if details.participants.is_empty() => {
            body = body.child(
                gpui::div()
                    .py(px(8.0))
                    .text_size(px(theme::TEXT_SIZE))
                    .text_color(theme::text_secondary())
                    .child("No participant data was returned"),
            );
        }
        Some(ConversationDetails::Group(details)) => {
            if details.permissions.can_manage_members() {
                let pending = this.group_mutation_in_progress;
                let blocked = pending || this.group_leave_uncertain;
                body = body.child(
                    gpui::div()
                        .id("add-group-members-row")
                        .min_h(px(48.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .text_size(px(theme::TEXT_SIZE))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(if blocked {
                            theme::text_secondary()
                        } else {
                            theme::accent_text()
                        })
                        .when(!blocked, |row| {
                            row.cursor_pointer()
                                .aria_label("Add group members")
                                .hover(|style| style.bg(theme::row_hover()))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.begin_add_group_members(window, cx)
                                }))
                        })
                        .child(
                            gpui::div()
                                .size(px(34.0))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(theme::row_selected())
                                .child(Icon::new(IconName::Plus).size(px(17.0))),
                        )
                        .child(if pending {
                            "Working…"
                        } else if this.group_leave_uncertain {
                            "Refresh group info"
                        } else {
                            "Add members"
                        }),
                );
            }
            for (index, participant) in details.participants.iter().enumerate() {
                body = body.child(participant_row(
                    participant,
                    details,
                    index,
                    this.group_leave_uncertain,
                    cx,
                ));
            }
        }
        _ if this.details_loading => {
            // Neutral skeletons communicate pending data without invented
            // names, roles, or participant counts.
            for width in [180.0, 220.0, 160.0] {
                body = body.child(
                    gpui::div()
                        .h(px(52.0))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            gpui::div()
                                .size(px(34.0))
                                .rounded_full()
                                .bg(theme::row_hover()),
                        )
                        .child(
                            gpui::div()
                                .w(px(width))
                                .h(px(10.0))
                                .rounded(px(5.0))
                                .bg(theme::row_hover()),
                        ),
                );
            }
        }
        _ => {
            body = body.child(
                gpui::div()
                    .py(px(8.0))
                    .text_size(px(theme::TEXT_SIZE))
                    .text_color(theme::text_secondary())
                    .child("Participant information is unavailable offline"),
            );
        }
    }
    body
}

fn participant_row(
    participant: &Participant,
    details: &wasabi_domain::GroupDetails,
    index: usize,
    actions_blocked: bool,
    cx: &mut Context<MainWindow>,
) -> gpui::Stateful<gpui::Div> {
    let initial = participant
        .display_name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let role = match participant.role {
        ParticipantRole::Member => None,
        ParticipantRole::Admin => Some("admin"),
        ParticipantRole::SuperAdmin => Some("creator"),
    };
    let actionable = details.permissions.can_manage_members()
        && !actions_blocked
        && !participant.is_self
        && participant.role != ParticipantRole::SuperAdmin;
    let target = crate::views::root::GroupMemberTarget {
        chat: details.chat.clone(),
        group_name: details.subject.clone(),
        participant: wasabi_domain::ChatId::new(participant.jid.clone()),
        participant_name: participant.display_name.clone(),
        participant_role: participant.role,
    };
    gpui::div()
        .id(("group-participant", index))
        .min_h(px(52.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .when(actionable, |row| {
            row.cursor_pointer()
                .aria_label(format!("Manage {}", participant.display_name))
                .hover(|style| style.bg(theme::row_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_group_member_actions(target.clone(), cx)
                }))
        })
        .child(
            gpui::div()
                .size(px(34.0))
                .rounded_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::sender_color(&participant.display_name))
                .text_color(theme::text_on_accent())
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(initial),
        )
        .child(
            gpui::div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::TEXT_SIZE))
                .text_color(theme::text_primary())
                .child(participant.display_name.clone()),
        )
        .children(role.map(|role| {
            gpui::div()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(4.0))
                .bg(theme::row_selected())
                .text_size(px(theme::TEXT_SIZE_SM))
                .text_color(theme::accent_text())
                .child(role)
        }))
        .when(actionable, |row| {
            row.child(
                Icon::new(IconName::ChevronRight)
                    .size(px(15.0))
                    .text_color(theme::text_secondary()),
            )
        })
}
