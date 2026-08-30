# What works today

This page describes the shipped behavior in wasabi `0.3.3`. It is a capability inventory, not a promise about unfinished controls. Features without a complete data source, failure state, and usable workflow stay hidden.

## Account and startup

| Status | Capability |
| --- | --- |
| Done | Link an account using a rotating QR code or a short-lived phone-number code. |
| Done | Reopen cached chats immediately while reconnecting in the background. |
| Done | Show pairing expiry, reconnect, and storage failures as specific in-product states. |
| Done | Surface forced logout, unlink, outdated-client, rate-limit, and temporary-ban states with specific recovery copy while keeping cached chats. |
| Done | Log out through a confirmation that distinguishes unlinking from local-data removal. |
| Done | Persist device settings independently from the linked account. |
| Done | Enable standards-based XDG autostart. |

## Chats and search

| Status | Capability |
| --- | --- |
| Done | Browse cursor-paginated active chats with All, Unread, device-local Favorites, and Groups filters plus Archived. |
| Done | Show a disk-cached contact or group photo on chat list rows and search hits when one is already stored; otherwise keep initials. |
| Done | Preserve protocol ordering: pinned chats first, followed by recent activity. |
| Done | Search loaded chats and the complete local message FTS index, then open the exact message in context. |
| Done | Search the cached address book, page through deterministic name ordering, and open existing contacts offline. |
| Done | Verify an international number with the linked account before starting an unknown-number chat. |
| Done | Create a real group from cached contacts with an acknowledged identity and duplicate-safe uncertain-delivery handling. |
| Done | Pin, mute, archive, and mark chats read or unread through synchronized controls. |
| Done | Clear or delete a chat through distinct, cancel-first confirmations naming the exact conversation. |

## Messages and composer

| Status | Capability |
| --- | --- |
| Done | Read cursor-paginated history and send text, image, video, audio, and document messages with supported captions. |
| Done | Measure multiline and multilingual bubbles from rendered layout so resizing and text scaling do not overlap. |
| Done | Preserve the visible history anchor across prepends and incoming messages, with an explicit jump-to-newest control. |
| Done | Keep independent, restart-safe drafts for each chat. |
| Done | Grow the composer to six visible lines; `Enter` follows the preference and `Shift+Enter` inserts a newline. |
| Done | Insert Unicode emoji from a local categorized composer picker at the caret; Escape, click-outside, or the toggle closes it. |
| Done | Bind sends and actions to the immutable chat identity captured at submission time. |
| Done | Keep failed outgoing rows visible and retry them under the same message identity. |
| Done | Restore composer text when a send fails before durable acceptance. |
| Done | Reply to text or media, persist reply context, render quotes, and navigate to the original message. |
| Done | Edit acknowledged outgoing text inside the protocol window with optimistic rollback. |
| Done | Copy, star, react to, delete locally, or revoke eligible sent messages through real protocol actions. |
| Done | Render reaction aggregates with counts and the linked account's own selection. |
| Done | Render received location and contact cards with a place name, address, and coordinates or a contact display name. Live locations are labeled without tracking; sharing those kinds from the composer is not available yet. |

## Media

| Status | Capability |
| --- | --- |
| Done | Stage outgoing files durably, recover interrupted attachments after restart, and cancel safely. |
| Done | Stream encryption and upload without buffering an entire file in memory. |
| Done | Download received media into a bounded, content-addressed, SHA-256-verified cache. |
| Done | Paint a still-image thumbnail for downloaded photos in the timeline; video, audio, and documents keep their existing cards. |
| Done | Configure the download location and actively enforced cache quota. |
| Done | Inspect cache usage and clear downloaded media through an explicit confirmation flow. |

## Contacts and groups

| Status | Capability |
| --- | --- |
| Done | Open direct-contact information on demand; direct conversations never display Participants. |
| Done | Block or unblock a direct contact when the live block state is known, and delete a saved contact name for a bare phone-number identity without deleting the chat. |
| Done | List cached groups in common on direct details when a stored group snapshot includes that contact; tapping opens the group. |
| Done | Refresh a direct contact's real About metadata when connected and retain the last authoritative value for honest offline viewing. |
| Done | Render a contact or group profile photo in the conversation header and information drawer when the linked account provides one, keeping initials when the photo is unavailable or restricted. |
| Done | Load real group subject, description, participant count, identities, and admin roles. |
| Done | Retain the last server-backed group snapshot for honest disconnected viewing. |
| Done | Edit group identity and permissions; add, promote, demote, or remove real participants; and leave through acknowledged, exact-target workflows. |
| Done | Review pending group join requests while connected, then approve or decline one person at a time through cancel-first confirmations. |
| Done | Get or reset a group invite link from details when you are a group admin. The URL is fetched live while connected, copied to the clipboard, and revoked through a cancel-first confirmation that names the group. |
| Done | Render unavailable or privacy-restricted data as unavailable instead of fabricating rows. |

## Desktop integration and settings

| Status | Capability |
| --- | --- |
| Done | Use a native, icon-only rail with accessible labels and hover tooltips. |
| Done | Keep the information drawer closed until explicitly opened; use an overlay and scrim at compact widths. |
| Done | Use light, dark, or Linux system appearance. |
| Done | Configure text size, Enter-to-send, notifications, download location, and media-cache quota with persistence. |
| Done | Receive Linux notifications respecting mute, focus, sound, and preview privacy. |
| Done | Focus the exact conversation when a notification is activated. |

## Intentionally not exposed yet

Calls, Status, Channels, and Communities remain hidden until their end-to-end workflows are ready. Remaining core-GA work is tracked in the [roadmap](../ROADMAP.md).
