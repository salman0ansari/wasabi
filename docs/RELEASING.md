# Releasing wasabi

## Version policy

wasabi uses Semantic Versioning:

- `0.MINOR.PATCH` is the pre-1.0 development series. A minor bump may contain breaking storage, API, or behavior changes. A patch bump is compatible within its minor line.
- `alpha.N` is incomplete and intended for contributors or deliberate testers.
- `beta.N` is feature-complete for the release but may still contain important defects.
- `rc.N` is a release candidate; only release-blocking fixes should land.
- `1.0.0` begins the stable compatibility promise.

The root `VERSION` file is authoritative. The root workspace and isolated desktop workspace repeat the value because Cargo does not support inheriting package metadata across nested workspaces. `scripts/check-release-metadata.sh` prevents drift.

## Development and preview cadence

- Commit each coherent, verified change when it reaches its own acceptance
  gate. Do not accumulate unrelated product work into one end-of-cycle commit.
- Keep commit subjects focused and use the Conventional Commit prefixes
  documented in `CONTRIBUTING.md`.
- Normally cut the next prerelease after 4–7 user-visible changes have landed
  under `Unreleased`. The range is a cadence, not a reason to split one feature
  artificially or hold an urgent security/recovery fix.
- A release commit contains version, changelog, and release metadata only; it
  does not absorb the preceding implementation commits.
- Security fixes and unusable-build regressions may trigger an immediate
  prerelease outside the normal batch.

## Release checklist

1. Choose the next version and update `VERSION`, both Cargo manifests, and the top changelog heading.
2. Run `./scripts/check-release-metadata.sh` and `./scripts/check-linux-packaging.sh`.
3. Run the root and desktop test suites and checks documented in the main README.
4. Run `benchmarks/desktop/measure-linux.sh` on the reference Linux machine and review regressions.
5. Verify light, dark, and system themes at the supported window sizes and text scales.
6. Verify fresh pairing, cached startup, reconnect, send retry, logout, and local-data retention/removal behavior using a dedicated test account.
7. Review logs and exported diagnostics for message content, phone numbers, pairing secrets, media keys, and unredacted account identifiers.
8. Update screenshots when production UI changed materially.
9. Commit the release metadata, create a signed `vVERSION` tag, and publish checksums with the release artifacts.
10. Start a new `Unreleased` section immediately after release.

No release is called stable merely because it builds. The acceptance gates in the product rebuild plan remain authoritative for the 1.0 decision.
