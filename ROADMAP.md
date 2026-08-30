# wasabi roadmap

wasabi is being built in public toward a dependable Linux-first replacement for browser-based WhatsApp use. A destination ships only when its data, mutations, loading, empty, offline, permission, and failure states are operational.

## Core foundation

| Status | Work |
| --- | --- |
| Done | Native GPUI shell, icon-only rail, responsive chat/list/drawer layout, light/dark/system themes, bundled Inter typography, reusable tokens, and honest states. |
| Done | Hydrate-first startup, durable SQLite projections, cancellation-aware background work, scoped invalidations, and deterministic shutdown. |
| In progress | Split the remaining root responsibilities into focused entities and reducers for routing, overlays, pairing, notifications, and transfers. |
| Planned | Central command registry generating bindings, tooltips, menus, and Keyboard Shortcuts from one source. |
| Planned | Accessibility pass for focus order, screen readers, contrast, reduced motion, and high text scaling. |

## Pairing, session, and recovery

| Status | Work |
| --- | --- |
| Done | QR pairing, phone-number codes, expiry/cancel/retry behavior, cached startup, reconnect state, and explicit logout. |
| Done | Specific forced-logout, client-outdated, rate-limit, and temporary-ban recovery surfaces. |
| Planned | History-sync recovery surfaces, linked-device management, relink diagnostics, corrupt-settings recovery UX, and resumable initial history progress. |

## Chats, search, and timeline

| Status | Work |
| --- | --- |
| Done | Active/archived pagination, filters, pin/mute/archive/read actions, drafts/favorites, FTS search, anchored navigation, and measured rows. |
| Done | Viewport-preserving prepends, non-jumping incoming messages, reply navigation, reactions, edits, starring, deletes, revoke, and clear. |
| Done | Render received location and contact cards in the timeline, quotes, chat-list previews, and notifications. Composer sharing and live-location tracking stay planned. |
| Done | Paint downloaded sticker stills in the timeline. Animated stickers are labeled and show a still frame, not a playing animation. |
| In progress | Complete content projection/rendering for polls, events, invites, voice/video notes, system, and remaining unsupported messages. |
| Planned | Grouped global search, in-chat next/previous, forwarding selection, receipt/reaction detail, typing transmission, and expiring presence. |

## Contacts and groups

| Status | Work |
| --- | --- |
| Done | Cached New Chat address book, verified unknown-number lookup, real group creation, distinct direct details with live/durable About metadata refresh, profile photos in the conversation header, information drawer, and chat list (disk-cached only), real group participants/admin roles, and offline snapshots. |
| Done | Real group creation, subject/description editing, add-member search, admin promotion/demotion, participant removal, permission controls, leave confirmation, truthful disconnected snapshots, and join-request review. |
| Done | Block, unblock, and delete-contact from direct details. Block state is live-only; delete contact removes the saved name without deleting the chat. Report is omitted because the protocol requires a message id. |
| Done | Get and reset a group invite link from details when the linked account is an admin. The URL is fetched live, copied to the clipboard, and revoked through a cancel-first confirmation. |
| Planned | Join via invite link, QR of the link, sharing into chats, and deeper role explanations. |

## Composer and media

| Status | Work |
| --- | --- |
| Done | Multiline text, per-chat drafts, reply/edit context, Enter-to-send, durable staging, multi-kind outbox, progress/cancel/retry, and verified cache. |
| Done | Local categorized Unicode emoji picker that inserts at the composer caret. |
| Done | Save As and Reveal in Files for media already in the local cache. |
| In progress | Live-account interoperability for each media class, metadata fidelity, visible thumbnail loading, and policy-driven auto-download. |
| Planned | Group mentions, voice recording, contact/location sharing, poll/event creation, and sticker creation. |

## Settings and Linux integration

| Status | Work |
| --- | --- |
| Done | Two-pane Settings shell, appearance/text/composer preferences, notification controls, cache usage/quota/path/clear, XDG autostart, and logout. |
| In progress | Complete General, Account, Privacy, Chats, Notifications, Storage and data, Shortcuts, and Help with typed persisted behavior. |
| Planned | Profile editing, supported synced privacy, blocklist, disappearing defaults, wallpaper, spellcheck, previews, archive/history management, media policies, licenses, and redacted diagnostics. |
| Planned | Tested Wayland support and common Linux packages; Windows and macOS follow Linux core GA. |
| Planned | Tray behavior only after it is reliable across supported desktops. |

## Reliability, performance, and releases

| Status | Work |
| --- | --- |
| Done | Separate headless/desktop suites, migrations, settings tests, release metadata checks, changelog, SemVer preview convention, reproducible benchmark, and redacted logging policy. |
| In progress | Deterministic UI fixtures, generation-race coverage, reconnect/outbox/transfer recovery, and lifecycle/descriptor soak expansion. |
| Planned | 20,000-chat/100,000-message dataset, 60 fps scroll gate, latency budgets, repeated soak tests, AppImage/Flatpak, signed checksums, reproducible artifacts, and update channel. |
| Planned | Security audit and privacy review before stable 1.0. |

## Core GA gates

| Status | Acceptance gate |
| --- | --- |
| Planned | Pairing and returning-session recovery pass on fresh and large accounts. |
| Planned | Text and every advertised media send survive brief disconnects with correct retry classification. |
| Planned | Direct and group details never invent identity or permission data. |
| Planned | Every advertised setting persists across restart and performs its real action. |
| Planned | Search, anchoring, notifications, destructive actions, and rapid chat switching pass race tests. |
| Planned | Supported sizes, themes, and 100/125/150% text scales have no clipping or overlaps. |
| Planned | Performance, lifecycle, descriptor, cache, and plaintext-log gates pass on the Linux reference machine. |

## Post-core parity modules

These rail destinations remain hidden until each module passes its functional and visual gate.

| Status | Module | Scope |
| --- | --- | --- |
| Planned | Calls | History/favorites, incoming calls, direct/group voice and video, device selection, screen sharing, links, and waiting rooms. |
| Planned | Status | List/viewer, muted updates, text/image/video publishing, deletion, and privacy controls. |
| Planned | Channels | Discovery/following, update feed, media, reactions, information, mute/unfollow/report. |
| Planned | Communities | Announcements, subgroups, membership, joining, creation, group linking/unlinking, and management. |
