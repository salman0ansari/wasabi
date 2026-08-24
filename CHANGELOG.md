# Changelog

All notable user-visible changes to Wasabi are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Cursor-based chat-list pagination with explicit load-more feedback and a real archived-conversations destination.
- Typed, immutable send requests that capture their destination chat before asynchronous work begins.
- Reproducible Linux desktop footprint benchmark and same-machine blank Electron baseline.
- Customer-facing project documentation and verified native screenshots.

## [0.2.0-alpha.1] - 2026-08-24

### Added

- Native GPUI desktop shell with a compact icon-only navigation rail.
- Chat-owned search and All, Unread, Favorites, and Groups filters.
- Responsive direct-contact and group information drawers.
- Two-pane Settings covering General, Account, Privacy, Chats, Notifications, Storage and data, Keyboard shortcuts, Help, and Log out.
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
