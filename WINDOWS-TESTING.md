# Windows test snapshot

Snapshot of the DPES-751 integration branch at commit a3d3d0116223e7a0d5b4f0b7e8549bc0960e56ce
(single squashed commit; history and CI workflows intentionally omitted).

Run on native Windows (MSVC toolchain, Rust stable, Visual Studio Build Tools with the C++ workload):

    cargo test -p mcao-backend-windows --locked
    cargo clippy -p mcao-backend-windows --all-targets --locked -- -D warnings
    cargo test -p mcao-core --locked

The Windows backend crate is `crates/mcao-backend-windows`; its tests are compiled only on Windows,
so they have never run on the macOS review host. Record pass/fail output per test binary
(lib, backend, conpty, interrupt, job). Known follow-ups: DPES-984 (minimum input window), DPES-983.
