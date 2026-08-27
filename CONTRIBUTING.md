# Contributing to wasabi

wasabi welcomes focused, well-tested contributions to the native Linux client.

## Before changing code

- Check the current `Unreleased` changelog and open work before starting a large feature.
- Keep protocol, database, and transport types behind product-facing boundaries.
- Do not add placeholder destinations or controls that cannot complete their advertised workflow.
- Never add real account databases, QR payloads, pairing codes, message content, media keys, or phone numbers to fixtures or logs.

## Quality bar

Run the root and isolated desktop checks from the README. Add deterministic tests for state and data changes. UI work should cover light and dark appearances, keyboard focus, narrow-window behavior, loading/empty/failure states, and screenshots when the visible product changes.

For deterministic visual inspection of received-media cards in a debug build:

```bash
WASABI_UI_PREVIEW=media cargo run --manifest-path apps/desktop/Cargo.toml
```

The preview is compiled out of release builds, uses fictitious identities and
metadata, does not hydrate account data, and performs no backend mutations.

Use Conventional Commit-style subjects where practical (`feat:`, `fix:`, `perf:`, `docs:`, `test:`, `refactor:`, `build:`). Commit each coherent change after its own verification instead of waiting to bundle unrelated work. User-visible changes belong in `CHANGELOG.md` under `Unreleased`.

By contributing, you agree that your contribution is provided under the repository's MIT license.
