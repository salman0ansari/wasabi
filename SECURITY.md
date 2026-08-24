# Security Policy

## Supported versions

Wasabi is currently pre-1.0. Security fixes are applied to the latest development release; older preview builds may not receive backports.

## Reporting a vulnerability

Do not publish an issue containing account data, pairing material, message content, media keys, or an exploitable security report. Contact the maintainers privately through the repository host's security-advisory feature. Include the affected version, platform, reproduction steps, impact, and any proposed mitigation.

Until a public security contact is configured, avoid using Wasabi for accounts whose compromise would create unacceptable risk. This project is an unofficial client under active development.

## Sensitive-data expectations

Normal logs and exported diagnostics must remain content-free and redact account identifiers. QR payloads, phone-number pairing codes, message bodies, phone numbers, media keys, and downloaded content must never enter normal logs.
