# Wasabi

**Current release: 0.2.0-alpha.1 — developer preview**

Wasabi is a fast, native Linux desktop messenger for WhatsApp accounts. It is built in Rust with GPUI and is designed to feel at home on the desktop without shipping an Electron runtime.

> **Developer preview:** Wasabi already supports pairing, cached conversations, message history, chat filtering, text and media messaging, responsive contact/group information, privacy-aware Linux notifications, message actions, and persistent desktop settings. Live-account media interoperability, deeper account management, and recovery hardening are still being completed before the first stable release.

## A focused Linux messenger

Wasabi is being built around a few straightforward principles:

- **Native and efficient.** A Rust and GPUI application instead of a bundled browser runtime.
- **Useful offline.** Cached chats and history appear immediately while the account reconnects.
- **Honest interfaces.** Wasabi does not invent contacts, participants, media, or unavailable features.
- **Familiar interaction.** The layout follows the current WhatsApp desktop information architecture while using original Wasabi branding and visuals.
- **Private by default.** Message bodies, phone numbers, pairing secrets, and media keys are excluded from normal logs.

## Screenshots

### Chats

![Wasabi chat workspace](docs/screenshots/chat-workspace-light.png)

### Attachments

Selected files are copied into restart-safe Wasabi staging before upload. The composer shows the real filename, media class, and size without exposing local paths.

![Wasabi durable attachment composer](docs/screenshots/attachment-composer-light.png)

### Settings

Settings are persistent and operational: the Storage surface reports live
cache use, enforces the selected quota, opens the Linux folder picker, and
requires confirmation before clearing downloaded media.

![Wasabi Storage settings in the light theme](docs/screenshots/settings-storage-light.png)

### Dark theme

![Wasabi Storage settings in the dark theme](docs/screenshots/settings-storage-dark.png)

### Responsive contact information

At the minimum supported window size, contact and group information opens over the conversation and can be dismissed with `Escape`.

![Wasabi responsive contact information drawer](docs/screenshots/contact-drawer-compact.png)

### Group information

When connected, group information is loaded on demand and uses real server metadata for the subject, description, participant count, identities, and admin roles.

![Wasabi group information drawer](docs/screenshots/group-info-light.png)

More verified captures are available in [`docs/screenshots`](docs/screenshots/README.md).

## What works today

- Link an account using a rotating QR code or a short-lived phone-number code.
- Reopen cached chats immediately while reconnecting in the background.
- Browse cursor-paginated active chats using All, Unread, device-local Favorites, and Groups filters, with a separate Archived destination.
- Search loaded chats and the complete local message FTS index with cancellable, paginated results that open the exact message in context.
- Read paginated message history and send text, image, video, audio, and document messages with optional supported captions.
- Stage outgoing files durably, recover interrupted composer attachments after restart, cancel them safely, and stream encryption/upload without buffering entire files in memory.
- Download received media on demand into a bounded, content-addressed, SHA-256-verified cache.
- Copy and react to messages, or star/unstar them with optimistic rollback if synchronization fails.
- Delete a message locally or revoke an eligible sent message through distinct, explicit confirmation dialogs.
- Pin/unpin and mute/unmute chats from the conversation drawer.
- Mark unread conversations read when they are opened in the active window, without cross-chat races.
- Keep independent per-chat text drafts across conversation switches and app restarts.
- Use light, dark, or Linux system appearance.
- Configure text size, Enter-to-send, notifications, download location, and an actively enforced media-cache quota.
- Inspect current media-cache usage and clear downloaded media through an explicit confirmation flow.
- Receive standard Linux desktop notifications that respect mute, focus, sound, and preview-privacy settings; clicking one focuses its conversation.
- Persist device settings independently from the linked account.
- Enable standards-based XDG autostart.
- Open direct-contact information without incorrect participant rows.
- When connected, load group subject, description, participant count, identities, and admin roles without fabricated rows.

## Native footprint

On the current Linux reference machine, five fresh-profile launches of Wasabi `0.2.0-alpha.1` opened the window in a mean 138.6 ms and settled at 232.7 MiB proportional set size (PSS). A blank Electron 41 window on the same machine took 523.8 ms and settled at 331.7 MiB PSS. Wasabi used one process versus Electron's six. Startup medians were 132 ms and 346 ms respectively; every raw sample is retained.

This is a reproducible runtime-baseline comparison, not a fabricated measurement of the official WhatsApp app. There is no official Linux desktop binary to measure locally, and Meta's current Windows and Mac apps should not be described as the old Electron client. Read the complete methodology, raw results, package-size context, limitations, and rerun instructions in [`benchmarks/desktop`](benchmarks/desktop/README.md).

## Before the stable release

The stable release is gated on live-account media interoperability testing, durable contact/group metadata refresh, remaining account controls, expanded recovery/interaction tests, and performance validation on large synchronized histories.

Calls, Status, Channels, and Communities remain hidden until their complete workflows are ready. Wasabi does not ship placeholder destinations.

## Run Wasabi from source

Wasabi currently targets Linux/X11. The repository pins the Rust toolchain it expects.

```bash
git clone https://github.com/salman0ansari/wasabi.git
cd wasabi
cargo run --manifest-path apps/desktop/Cargo.toml
```

GPUI requires the standard Linux X11, font, graphics, and audio development libraries supplied by your distribution. Wayland, Windows, and macOS portability is planned, but those platforms do not currently block the Linux release.

Wasabi stores account data under the platform data directory (normally `~/.local/share/wasabi`) and device preferences in `~/.config/wasabi/settings.json`.

## Development checks

The headless workspace and native desktop workspace are intentionally isolated:

```bash
cargo test --workspace --all-targets
cargo test --manifest-path apps/desktop/Cargo.toml --all-targets
cargo check --manifest-path apps/desktop/Cargo.toml --all-targets
./scripts/check-release-metadata.sh
```

## Versioning and releases

Wasabi follows [Semantic Versioning](https://semver.org/). Until 1.0, minor releases may make breaking changes while patch releases remain compatible within that minor line. Preview builds use explicit identifiers such as `alpha`, `beta`, and `rc`.

The repository's [`VERSION`](VERSION) file is the release source of truth. See the [`CHANGELOG`](CHANGELOG.md) for product changes and [`docs/RELEASING.md`](docs/RELEASING.md) for the release process.

Contributions are welcome; start with [`CONTRIBUTING.md`](CONTRIBUTING.md). Please report security-sensitive issues using the private process in [`SECURITY.md`](SECURITY.md), not a public issue.

## Unofficial client notice

Wasabi is an independent project and is not affiliated with, endorsed by, or sponsored by WhatsApp or Meta. WhatsApp is a trademark of its respective owner.
