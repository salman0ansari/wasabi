# wasabi

**Current release: 0.3.1 — developer preview**

wasabi is a fast, native Linux desktop messenger for WhatsApp accounts. It is built in Rust with GPUI and is designed to feel at home on the desktop without shipping an Electron runtime.

> **Developer preview:** wasabi already supports pairing, cached conversations, message history, chat filtering, text and media messaging, responsive contact/group information, privacy-aware Linux notifications, message actions, and persistent desktop settings. Live-account media interoperability, deeper account management, and recovery hardening are still being completed before the first stable release.

## A focused Linux messenger

wasabi is being built around a few straightforward principles:

- **Native and efficient.** A Rust and GPUI application instead of a bundled browser runtime.
- **Useful offline.** Cached chats and history appear immediately while the account reconnects.
- **Honest interfaces.** wasabi does not invent contacts, participants, media, or unavailable features.
- **Familiar interaction.** The layout follows the current WhatsApp desktop information architecture while using original wasabi branding and visuals.
- **Private by default.** Message bodies, phone numbers, pairing secrets, and media keys are excluded from normal logs.

## Product documentation

- [Screenshot gallery](docs/screenshots/README.md) — verified captures kept out of this compact landing page.
- [What works today](docs/WHAT-WORKS.md) — an honest, release-specific capability inventory.
- [Roadmap](ROADMAP.md) — the complete path from the current preview to core GA and later parity modules.
- [Desktop benchmarks](benchmarks/desktop/README.md) — methodology, raw samples, limitations, and rerun instructions.

## Native footprint

On the current Linux reference machine, five fresh-profile launches of wasabi `0.2.0-alpha.3` had a 118 ms startup median and settled at 233.5 MiB proportional set size (PSS). A blank Electron 41 window had a 467 ms median and settled at 321.1 MiB PSS. wasabi used one process versus Electron's six. A retained 3,730 ms first wasabi launch raised its mean to 840.2 ms versus Electron's 527.8 ms; all four subsequent wasabi samples were 117–118 ms. Every raw sample is committed.

This is a reproducible runtime-baseline comparison, not a fabricated measurement of the official WhatsApp app. There is no official Linux desktop binary to measure locally, and Meta's current Windows and Mac apps should not be described as the old Electron client. Read the complete methodology, raw results, package-size context, limitations, and rerun instructions in [`benchmarks/desktop`](benchmarks/desktop/README.md).

## Before the stable release

The stable release is gated on live-account media interoperability testing, remaining group/account controls, expanded recovery/interaction tests, and performance validation on large synchronized histories.

Calls, Status, Channels, and Communities remain hidden until their complete workflows are ready. wasabi does not ship placeholder destinations.

## Run wasabi from source

wasabi currently targets Linux/X11. The repository pins the Rust toolchain it expects.

```bash
git clone https://github.com/salman0ansari/wasabi.git
cd wasabi
cargo run --manifest-path apps/desktop/Cargo.toml
```

GPUI requires the standard Linux X11, font, graphics, and audio development libraries supplied by your distribution. Wayland, Windows, and macOS portability is planned, but those platforms do not currently block the Linux release.

wasabi stores account data under the platform data directory (normally `~/.local/share/wasabi`) and device preferences in `~/.config/wasabi/settings.json`.

## Development checks

The headless workspace and native desktop workspace are intentionally isolated:

```bash
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
(cd apps/desktop && cargo fmt --all -- --check)
cargo check --locked --manifest-path apps/desktop/Cargo.toml --all-targets
cargo test --locked --manifest-path apps/desktop/Cargo.toml --all-targets
./scripts/check-release-metadata.sh
./scripts/check-linux-packaging.sh
```

## Versioning and releases

wasabi follows [Semantic Versioning](https://semver.org/). Until 1.0, minor releases may make breaking changes while patch releases remain compatible within that minor line. Preview builds use explicit identifiers such as `alpha`, `beta`, and `rc`.

The repository's [`VERSION`](VERSION) file is the release source of truth. See the [`CHANGELOG`](CHANGELOG.md) for product changes and [`docs/RELEASING.md`](docs/RELEASING.md) for the release process.

Contributions are welcome; start with [`CONTRIBUTING.md`](CONTRIBUTING.md). Please report security-sensitive issues using the private process in [`SECURITY.md`](SECURITY.md), not a public issue.

## Unofficial client notice

wasabi is an independent project and is not affiliated with, endorsed by, or sponsored by WhatsApp or Meta. WhatsApp is a trademark of its respective owner.
