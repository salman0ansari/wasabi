# Wasabi Linux desktop benchmark

This benchmark compares Wasabi with a deliberately blank Electron window on the same Linux/X11 machine. It is designed to answer one narrow question: what is the process and resource cost of Wasabi's native desktop stack versus starting an Electron runtime before application features are added?

It is **not** a benchmark of the official WhatsApp application. Meta does not publish an official Linux desktop binary; its download page directs unsupported desktop users to WhatsApp Web. Meta also replaced its older desktop clients with faster Windows and Mac applications, so describing the current official desktop apps as Electron would be inaccurate.

## Latest reference result

Measured on 2026-08-24 with Wasabi `0.2.0-alpha.1` and Electron `41.3.0`. Values are means across five fresh-profile launches.

| Metric | Wasabi | Blank Electron | Wasabi difference |
|---|---:|---:|---:|
| Window startup (mean) | 159.8 ms | 546.0 ms | 70.7% faster |
| Window startup (median) | 131 ms | 360 ms | 63.6% faster |
| RSS after settle | 256.8 MiB | 619.4 MiB | 58.5% lower |
| PSS after settle | 222.1 MiB | 319.8 MiB | 30.6% lower |
| Idle CPU sample | 1.00% | 0.07% | Electron lower in this run |
| Processes | 1.0 | 6.0 | 5 fewer |
| Threads | 55.4 | 72.8 | 23.9% fewer |
| File descriptors | 77.0 | 252.0 | 69.4% fewer |

RSS double-counts shared mappings across processes; PSS apportions shared pages and is the better whole-tree memory comparison. Both are included so the raw behavior stays visible. The Electron case renders only a local heading, while Wasabi initializes its native UI, local storage, protocol/session machinery, and pairing state. Conversely, this is a fresh unpaired profile, not a large synchronized account. Treat the result as a repeatable baseline, not a universal promise.

The release profile measured here produced a symbol-stripped 68,391,104-byte executable (65.2 MiB), including the native notification and media subsystems. On this Arch Linux machine the Electron runtime executable alone was 204,037,104 bytes (194.6 MiB), and `/usr/lib/electron41` occupied about 294 MiB before any WhatsApp application code or profile data.

The first launch in this run was colder for both applications (272 ms for Wasabi and 1,326 ms for Electron), so the table includes startup medians alongside the script's committed mean summary. No sample was discarded.

## Method

`measure-linux.sh` performs five runs by default. Each run:

1. Creates isolated XDG data, config, and cache directories for both applications.
2. Starts the process in a new process group.
3. Uses X11 window ownership to measure startup until the real titled window exists.
4. Waits eight seconds for initialization to settle.
5. Samples aggregate CPU ticks for three seconds.
6. Walks every descendant process and totals RSS, PSS, threads, open file descriptors, and process count.
7. Terminates and reaps the complete process group.

Run it after producing a release build:

```bash
cargo build --manifest-path apps/desktop/Cargo.toml --release
./benchmarks/desktop/measure-linux.sh
```

Requirements are an active X11 session, `xdotool`, and an Electron executable on `PATH`. Override paths and timings with `WASABI_BIN`, `ELECTRON_BIN`, `RUNS`, `SETTLE_SECONDS`, `CPU_SAMPLE_SECONDS`, `OUTPUT`, and `ENVIRONMENT_OUTPUT`.

The committed raw samples are in `results-linux.csv`; machine metadata is in `environment-linux.txt`. Re-run before releases and investigate meaningful regressions instead of replacing an unfavorable result without explanation.

## Official product context

- [WhatsApp's official download page](https://www.whatsapp.com/download) routes unsupported devices to the browser.
- [Meta's 2023 Windows announcement](https://about.fb.com/news/2023/03/faster-speeds-improved-calling-whatsapp-desktop/) describes a replacement app focused on faster loading and improved multi-device behavior.
- [Meta's 2023 Mac announcement](https://about.fb.com/news/2023/08/new-whatsapp-app-for-mac-group-calling/) describes the corresponding improved Mac experience.

Wasabi's product goal is therefore precise: provide Linux with a capable native desktop client and continuously measure its footprint against a transparent browser-runtime baseline. Claims about official Windows or macOS resource usage require separate same-machine measurements on those operating systems.
