# Changelog

All notable user-visible changes to wasabi are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Starred messages can be browsed from the chat list header, opened in their
  original conversation, and unstarred from that viewer. The list reads
  durable starred rows, newest first, and hides revoked messages.

### Changed

- Refined the native desktop shell and messaging surfaces with a denser,
  WhatsApp-inspired interaction system: neutral light/dark layers, 64 px pane
  rhythm, inset chat rows, grouped directional message bubbles, compact inline
  delivery metadata, a unified composer, elevated emoji tray and dialogs, and
  medium-width contact-info takeover behavior. A new Reduce motion preference
  disables tray and dialog entrance effects. Wasabi branding and existing
  messaging behavior remain unchanged.

## [0.3.6] - 2026-08-31

### Added

- Received polls and quizzes now render as cards with the real question and
  option names. Quizzes are labeled without revealing the correct answer.
  Voting and poll creation are not available yet.

## [0.3.5] - 2026-08-31

### Added

- Downloaded stickers now paint a still image in the timeline from the local
  cache, using the same verified thumbnail path as photos. Animated stickers
  keep the Animated sticker label and show a still frame rather than playing.

## [0.3.4] - 2026-08-31

### Added

- Downloaded photos, videos, audio, documents, and stickers can be saved to a
  chosen path or revealed in the file manager. Wasabi copies from the local
  cache and keeps the cache entry; a missing cache file says so and returns
  the message to download instead of claiming the file is still present.

## [0.3.3] - 2026-08-31

### Added

- Received location and contact messages now render as dedicated cards instead
  of “Unsupported message”. Location cards show a name, address, and coordinates
  when present, and live locations are labeled without tracking or map tiles.
  Contact cards show the shared display name or an honest Contact/Contacts
  label. Quotes, chat-list previews, and notifications use the same labels.
  Sharing location or contacts from the composer is not available yet.

## [0.3.2] - 2026-08-31

### Added

- Composer now has a local categorized Unicode emoji picker next to attach.
  Clicking an emoji inserts it at the caret and leaves the draft focused;
  Escape, click-outside, or the toggle closes the picker.

## [0.3.1] - 2026-08-31

### Added

- Session recovery now names forced logout, a normal unlink, a client WhatsApp
  no longer accepts, a temporary restriction (with its wait window), and device
  rate-limiting. Cached chats stay on disk. Pairing 429 responses tell the user
  to wait rather than dumping protocol errors.

### Fixed

- Temporary-ban protocol events now fail the session instead of being dropped
  by the event pump, so the restriction is visible.
- Connect-failure reasons no longer reach the UI as Debug dumps.

## [0.3.0] - 2026-08-30

### Added

- Group admins can now copy or reset the live invite link from group details.
  The URL is fetched while connected, never stored locally, and reset only
  after a cancel-first confirmation that names the group and explains that
  anyone with the old link can no longer join.

## [0.2.0-alpha.9] - 2026-08-30

### Added

- Direct contact details now list groups in common from the local group cache.
  The section appears only when a cached group snapshot includes this contact,
  shows each group's real subject, and opens that chat on tap. Incomplete
  history never claims there are none.

## [0.2.0-alpha.8] - 2026-08-30

### Added

- Direct contact details can now block, unblock, or delete a saved contact
  name. Block and unblock appear only when the live block state is known;
  delete contact requires a bare phone-number identity, removes the address-book
  name, and leaves the chat in place. Report is not offered because it needs a
  message id.

## [0.2.0-alpha.7] - 2026-08-30

### Added

- Group admins can now review pending join requests from group details.
  Requests are queried live while connected, approved or declined one at a
  time through cancel-first confirmations that name the exact person and
  group, and dropped from the pending list after a successful change.

## [0.2.0-alpha.6] - 2026-08-30

### Added

- Downloaded photos in the conversation timeline now show a still-image
  thumbnail instead of a generic downloaded placeholder. Video, audio, and
  documents keep their existing cards.

## [0.2.0-alpha.5] - 2026-08-30

### Added

- Conversation headers and contact/group information now render the real
  profile photo when the linked account provides one. Photos are fetched as
  previews, stored in the existing media cache, and fall back to initials when
  the picture is unavailable, privacy-restricted, or the session is offline.
- Chat list rows and message search hits now show a disk-cached profile photo
  when one is already stored for that contact or group. Missing cache entries
  keep initials; the list never fetches pictures itself.

## [0.2.0-alpha.4] - 2026-08-30

### Added

- Direct-contact information now refreshes real About/avatar metadata from the
  linked account when connected and persists the last authoritative snapshot
  for offline details. A successful privacy/unavailable response clears stale
  cached fields instead of continuing to display old metadata.
- Linux releases now have a canonical freedesktop application-menu entry and
  a deterministic packaging metadata check that is exercised by root CI.

### Fixed

- Restart recovery now waits for the first connected state and reconciles
  durable pre-launch Pending sends exactly once. The sweep walks selected chat
  history to exhaustion, so an old unsent row is no longer skipped behind a
  long run of later acknowledged messages.
- Received image/video/PTV media that carries a protocol `static_url` now stays
  on whatsapp-rust's typed verbatim-URL download path instead of losing that
  routing metadata while being projected into host-routed download parameters.
- Active and archived chat pagination now use Wasabi-owned partial keyset
  indexes for their actual filters. Archived pages query archived rows directly
  rather than scanning and discarding thousands of unrelated active chats.
- Core-owned task accounting now releases its shutdown-drain count on normal
  completion, cancellation, abort, or panic, preventing a failed background
  future from holding the drain open until timeout.
- Opening a database written by a newer Wasabi schema now fails before any
  migration mutation instead of overwriting its version with an older one.

## [0.2.0-alpha.3] - 2026-08-30

### Added

- Group admins can now add real members from the information drawer through a
  searchable, paginated cached-contact picker. Existing participants are
  excluded, the exact group and selected identities are captured before the
  request, partial server rejections are reported without inventing success,
  and acknowledged metadata refreshes the participant list before the picker
  closes.
- Admins can now open real participant actions from group details, promote a
  member, dismiss an admin, or remove a participant through exact-person,
  exact-group cancel-first confirmations. The signed-in user and group creator
  remain non-actionable, stale roles are rejected before dispatch, partial
  protocol rejection is surfaced, and successful metadata refreshes the real
  participant roles.
- Group members can now leave through an exact-group, cancel-first
  confirmation that explains local history retention. Wasabi waits for remote
  acknowledgement, closes stale detail state, atomically deletes the cached
  group/participant snapshot, suppresses any failed-cleanup snapshot for the
  rest of the process, and allows a genuine later re-add to refresh metadata.
- Group details now expose acknowledged, admin-gated controls for who may edit
  group information, who may send messages, and whether new members require
  approval. Mutations capture an immutable group identity, use typed protocol
  operations, refresh and cache real server metadata after success, discard
  stale results after navigation, and explain missing admin permission without
  enabling an inert control. The same redacted product boundary now supports
  subject, description, member, role, and leave operations for their upcoming
  UI workflows.
- Group admins can now edit the real group name or multiline description from
  the information drawer. Both editors preload the current server value,
  enforce protocol character limits, treat an empty description as removal,
  retain validation errors in the modal, and apply the update through the same
  chat-bound acknowledged mutation path.

### Changed

- Refreshed the reproducible Linux native-versus-blank-Electron benchmark for
  this release. Five fresh-profile runs retain the cold-start outlier, report
  both mean and median startup, and record the complete process-tree resource
  samples without claiming to measure an unavailable official Linux client.

## [0.2.0-alpha.2] - 2026-08-27

### Added

- Inter 4.1 is now bundled under the SIL Open Font License and applied across
  every GPUI surface, including after live theme changes. This replaces
  distribution-dependent UI typography while leaving unsupported scripts and
  emoji to the platform fallback stack.
- Customer documentation is now split into a compact landing page, a
  screenshot gallery, an honest `What works today` inventory, and a complete
  status-labelled roadmap.
- New Group now provides searchable multi-selection over the real cached
  address book, a validated subject step, immutable participant capture, and
  one typed server creation command. Acknowledged groups are inserted using
  their real server JID and creation time without a fake message; ambiguous
  transport outcomes block blind retry so an already-created group is not
  duplicated. Successful and on-demand group metadata snapshots, including
  actual participants and signed-in admin role, are atomically cached for
  truthful disconnected drawer rendering.
- New Chat can now validate an international number against the live linked
  account before offering Start chat. The typed lookup boundary redacts its
  input/result diagnostics, persists upstream PN/LID resolution, distinguishes
  offline, timeout, rate-limit, rejection, and not-registered outcomes, and
  drops stale results after query, modal, navigation, logout, or connection
  changes without inserting synthetic contacts or chat timestamps. Formatted
  numbers are canonicalized for the local cache first, avoiding redundant live
  checks for already-saved contacts.
- New Chat now searches real cached direct contacts through a typed,
  repository-owned keyset query with deterministic name/JID ordering, PN/LID
  alias merging, literal wildcard handling, cached avatars, bounded pages,
  debounced generation cancellation, offline/empty/error states, and contact
  invalidation refresh.
  Selecting a contact opens a real empty conversation without fabricating a
  chat row or timestamp; the modal scrim prevents click-through.
- Conversation information now exposes synchronized archive/unarchive and
  mark-read/unread controls alongside pin and mute. Clear and delete use
  separate immutable protocol commands, exact-chat cancel-first dialogs,
  preserve starred/downloaded content by default, suppress duplicate submits,
  and change the local conversation only after protocol acceptance.
- The composer now grows from one to six measured lines, preserves restored
  multilingual cursor positions, applies text-size preferences to its editor
  geometry, submits with plain Enter only when configured, and always treats
  Shift+Enter as a newline without duplicate insertion.
- Message bubbles now project durable reactions into per-emoji counts, mark
  the linked account's own choice, and expose compact toggle chips. Reaction
  events remain hidden from the timeline; optimistic replace/remove updates
  roll back on failure, and successful sends materialize locally only after
  protocol acceptance.
- Acknowledged outgoing text messages can now be edited inside the protocol
  window. The action captures its immutable chat/message identity, persists
  edit mode in the per-chat draft, refuses to overwrite active composer work,
  sends a real WhatsApp edit, materializes accepted content in place, and
  rolls the optimistic bubble back while retaining retryable text on failure.
- Message actions now start real WhatsApp-compatible replies for text and
  attachments. Reply targets persist in per-chat drafts, outgoing context is
  built from the exact durable original message, received quotes render as
  bounded safe projections, and quoted cards navigate to anchored history.
- Text failures before the durable commit barrier now restore an untouched
  composer draft; failures after the barrier stay cleared because the durable
  failed bubble owns same-ID Retry. Compound audio-plus-text sends clean up an
  accepted attachment independently from a failed follow-up text send.
- Failed outgoing messages now expose inline and message-menu Retry actions.
  Retry republishes the durable stored proto under its original message ID,
  rejects incoming/non-failed/missing-content rows, prevents duplicate clicks,
  and refreshes the exact conversation after completion.
- The conversation timeline now uses GPUI's native measured variable-height
  list, so multiline, multilingual, emoji, media, resized, and scaled content
  is laid out from actual rendered geometry instead of character estimates.
- Stable date/message item identities preserve the same visible message and
  pixel offset across prepends, bounded-window churn, and remeasurement.
- Incoming messages no longer evict history while it is being read; a compact
  new-message affordance moves to the newest anchored page on demand.
- Account settings now provide a protocol-backed logout flow with an explicit
  cancel-first confirmation, preserve cached local account data, clear live UI
  state, and return the desktop to secure pairing.
- Every icon-only navigation-rail control now has a hover tooltip, AccessKit
  label, and accurate selected state without adding visible rail names.
- Storage settings now report live media-cache usage, enforce persisted
  256 MiB/1 GiB/4 GiB quotas, open the native Linux directory picker, and
  clear downloaded media through an explicit confirmation flow.
- Device-settings loading now recovers safely from corrupt JSON and
  normalizes invalid persisted text-scale, quota, version, and path values.
- Received media projections now retain display-safe MIME type, file size,
  dimensions, duration, filename, voice/video-note state, and availability.
- Added opaque media identities so UI and service APIs never carry CDN paths,
  encryption keys, hashes, or raw media bytes.
- Received media can be downloaded on demand into a bounded,
  content-addressed cache with SHA-256 verification, reconnect-safe client
  resolution, deduplicated in-flight work, and retry feedback on the message.
- Undecryptable, view-once, hosted, and bot messages now render distinct,
  actionable explanations instead of one generic unsupported-message label.
- Composer typing state now sends throttled, immutable chat-bound composing
  updates and an automatic paused update after inactivity or a chat switch.
- Incoming typing and voice-recording activity now appears in chat rows and
  the open conversation header, expires automatically, and clears on feed lag
  or disconnect instead of becoming stale durable state.
- Durable upload/download jobs now preserve opaque identity, exact Linux
  paths, byte progress, redacted failure class, and restart-safe lifecycle
  state; stale callbacks cannot regress progress or resurrect terminal jobs.
- Outgoing attachment encryption now streams through a disk-backed staging
  file with bounded upload admission, cancellation, reconnect-time client
  resolution, and constant memory use instead of buffering whole files.
- Account schema v2 stores restart-safe attachment kind, display name, MIME
  type, and caption metadata; the additive v1 migration preserves existing
  transfer rows.
- Outgoing attachment sources can now be copied into fsync-backed Wasabi staging with
  cancellation cleanup and a two-GiB safety ceiling; durable stages are kept
  separate from evictable received-media cache entries and temporary files.
- Attachment captions and metadata now update atomically on one opaque
  transfer job, while terminal jobs reject stale composer rewrites.
- Typed attachment sends now enforce their captured chat identity, persist
  captions, stream encrypted uploads, build image/video/audio/document
  messages, publish through the durable outbox, retain retryable plaintext,
  and erase staged plaintext only after the message is durably recorded.
- The composer now opens the Linux XDG file portal, shows honest preparation
  and per-chat attachment cards, prevents duplicate sends, supports removal,
  and recovers staged or interrupted attachments after restart.

- Phone-number account linking with validated international numbers, short-lived eight-character codes, expiry countdown, cancellation, and redacted sensitive values, alongside the existing QR flow.
- Visible, focused conversation selection now synchronizes read state through an immutable chat-bound command, with optimistic rollback on failure.
- Message search results now open a bounded durable context around the exact result, center it in the timeline, and apply a visible accent outline.
- Message action sheets now expose quick reactions, text copy, star/unstar, delete-for-me, and eligible delete-for-everyone actions; destructive paths require an explicit, message-specific confirmation and Escape cancels safely.
- Linux desktop notifications now honor global enablement, sound, preview privacy, focused-window suppression, outgoing-message suppression, and protocol mute state; clicking opens the exact chat.
- Per-chat text drafts with 400 ms durable saves, chat-bound generation guards, restoration on conversation switch, visible `Draft:` previews, and a final save before shutdown.
- Immutable message-action commands for star, react, delete-for-me, and revoke-for-everyone, with a working optimistic star/unstar bubble action.
- Immutable chat-action commands for pin, mute, archive, and read state, with working optimistic pin/mute drawer controls.
- Additive account migrations for device-local chat preferences, contact/group metadata caches, participant caches, and durable transfer jobs; Favorites now persist separately from protocol pinning.
- Typed direct-contact and group information projections, on-demand metadata loading with stale-result cancellation, and real group participant/admin rows when connected.
- Debounced, generation-cancelled global message search backed by the account FTS index, shown alongside matching loaded chats.
- Cursor-based chat-list pagination with explicit load-more feedback and a real archived-conversations destination.
- Typed, immutable send requests that capture their destination chat before asynchronous work begins.
- Reproducible Linux desktop footprint benchmark and same-machine blank Electron baseline.
- Customer-facing project documentation and verified native screenshots.

### Changed

- The visible product name is consistently lowercase `wasabi` in the native
  window title, chat heading, Settings, Linux notifications, and XDG autostart
  entry.
- Captures containing personal contact data were removed from the current tree
  and rewritten out of branch history. The screenshot gallery now requires
  synthetic identities and content.

- Removed visible diagnostics, spellcheck, link-preview, automatic-download,
  disappearing-message, and blocklist controls until each has a working
  product service; Settings no longer promises behavior it cannot execute.
- The configured cache quota is now applied when the media service opens,
  instead of being a display-only preference.
- Normal history, anchored search history, notifications, and global search now
  share one protocol-to-product message projection.
- Removed unverified placeholder values for shared media, starred messages,
  notification overrides, disappearing messages, and groups in common from the
  conversation information drawer until their real data sources are wired.
- Refreshed the reproducible native-versus-blank-Electron benchmark after
  adding notifications, downloads, typing, durable uploads, attachments,
  measured timelines, safe retry, and protocol replies; raw samples and
  cold-start outliers remain committed.
- GPUI now depends on a mockable `DesktopBackend` product-service contract instead of the concrete protocol bridge.

## [0.2.0-alpha.1] - 2026-08-24

### Added

- Native GPUI desktop shell with a compact icon-only navigation rail.
- Chat-owned search and All, Unread, Favorites, and Groups filters.
- Responsive direct-contact and group information drawers.
- Two-pane Settings covering General, Account, Privacy, Chats, Notifications, Storage and data, Keyboard shortcuts, and Help.
- Persistent light, dark, and Linux system themes, text sizing, send behavior, notification preferences, download location, cache quota, and XDG autostart.
- Honest pairing, loading, empty, offline, and unavailable states without invented contacts or participant data.

### Changed

- Removed the closure-based text transport seam and volatile pending string queue; the desktop bridge now resolves the live client internally and routes requests directly through the durable outbox.
- Configured release builds to strip compiler symbol tables from shipped binaries.
- Rebuilt the desktop information architecture around the current unified WhatsApp Web/Desktop interaction model while retaining original Wasabi branding.
- Removed the global search/network toolbar, persistent information drawer, placeholder destinations, fake participants, alphabetical chat sorting, and overly bright selected-state treatment.
- Scoped repository invalidations to affected chats, messages, or contacts rather than refreshing every list for every store event.
- Improved chat-row geometry and conservative timeline measurement to prevent overlapping content.

### Fixed

- Direct conversations no longer display a Participants section.
- Selecting a different chat now closes the information drawer.
- Narrow windows use a dismissible overlay drawer instead of permanently crushing the conversation.

[Unreleased]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.9...HEAD
[0.2.0-alpha.9]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.8...v0.2.0-alpha.9
[0.2.0-alpha.8]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.7...v0.2.0-alpha.8
[0.2.0-alpha.7]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.6...v0.2.0-alpha.7
[0.2.0-alpha.6]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.5...v0.2.0-alpha.6
[0.2.0-alpha.5]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.4...v0.2.0-alpha.5
[0.2.0-alpha.4]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.3...v0.2.0-alpha.4
[0.2.0-alpha.3]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.2...v0.2.0-alpha.3
[0.2.0-alpha.2]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.1...v0.2.0-alpha.2
[0.2.0-alpha.1]: https://github.com/salman0ansari/wasabi/releases/tag/v0.2.0-alpha.1
