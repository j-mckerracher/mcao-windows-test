# Agent ready

- UTC time: 2026-11-02 22:54:32 UTC
- Windows version: Microsoft Windows NT 10.0.26200.0 (Microsoft Windows 11 Education, build 26200, AMD64)
- git: `git version 2.46.2.windows.1`
- rustc (pinned by rust-toolchain.toml in the test checkout): `rustc 1.95.0 (59807616e 2026-04-14)`, host `x86_64-pc-windows-msvc`
- cargo: `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`
- gh on PATH: `yes`

Full version output (redaction check: contains no user or machine names):

```
rustc 1.95.0 (59807616e 2026-04-14)
binary: rustc
commit-hash: 59807616e1fa2540724bfbac14d7976d7e4a3860
commit-date: 2026-04-14
host: x86_64-pc-windows-msvc
release: 1.95.0
LLVM version: 22.1.2
cargo 1.95.0 (f2d3ce0bd 2026-03-21)
gh version 2.89.0 (2026-03-26)
https://github.com/cli/cli/releases/tag/v2.89.0
git version 2.46.2.windows.1
```

Runner: agent-driven verification loop on the managed Windows 11 laptop.
Test checkout path used: `C:/Users/<user>/AppData/Local/mcao-winverify/src`
Results checkout path used: `C:/Users/<user>/AppData/Local/mcao-winverify/push`
Shared build cache: `C:/Users/<user>/AppData/Local/mcao-winverify/target` (set as `CARGO_TARGET_DIR` for every cargo command)
