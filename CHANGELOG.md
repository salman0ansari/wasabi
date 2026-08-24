# Changelog

All notable user-visible changes to Wasabi are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

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
  adding notifications, downloads, typing, durable uploads, and the attachment
  composer; raw samples and cold-start outliers remain committed.
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

[Unreleased]: https://github.com/salman0ansari/wasabi/compare/v0.2.0-alpha.1...HEAD
[0.2.0-alpha.1]: https://github.com/salman0ansari/wasabi/releases/tag/v0.2.0-alpha.1
