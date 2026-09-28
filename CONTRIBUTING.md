# Development

Build requirements and installation are in [docs/installation.md](docs/installation.md).
Use the checked-in lockfile. The app, IPC crate, layer and optional sensor reader
form one workspace; checking only the desktop binary misses overlay regressions.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --workspace --locked
```

The CI workflow is configured to check Rust 1.92 and build/test on x86_64 and
aarch64. An architecture build is not proof of game/driver compatibility. Shader compilation requires
`glslangValidator` (Debian/Ubuntu package `glslang-tools`).

For screenshots, run `argus-lasso --ui-tour <output-directory>`. This isolated
preview uses read-only collection: it must never enforce policies, bind the
production overlay socket or save configuration. Root captures do not include
native child windows; photograph those separately. Review images for private
information before publishing. `examples/overlay-settings-preview.rs` renders the
actual customization controls without a daemon or configuration writes.
The tour warms up by frame count: a fast headless renderer can time out before
three sensor snapshots arrive. Treat its nonzero exit and warmup warning as a
failed capture, even if PNGs exist. On the development host, Xvfb with
`LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=softpipe` allowed warmup to complete.
Preview CPU readings include capture overhead and are not benchmarks.

Keep sensor sampling, text composition, graph refresh, frame collection and disk
writes separate. Never replace missing telemetry with zero or TDP. Protocol
changes require matching daemon/layer installs and an explicit protocol version.
Use the paired installer; inspect the running process's mapped library and build
IDs. Do not overwrite a mapped library in place.

Renderer changes need native Vulkan synchronization validation and the three-case
comparison in [docs/overlay.md](docs/overlay.md). A build, a synthetic benchmark or
a user report alone is not a controlled game-performance result. State which
Steam/Proton/driver paths were actually tested.

Update current guides, the Unreleased changelog and affected screenshots together.
Record dated verification in [docs/status.md](docs/status.md). Distinguish package
version, source commit, installed build ID and published release; they are not
interchangeable. Label earlier measurements/screenshots with their actual date
and scope. Verify release claims against GitHub rather than inferring from Cargo.
Follow [the GUI style conventions](docs/ui-style.md) when changing interface text,
fonts, colors or layout.
Keep dated investigation notes in `docs/archive/`; do not present old test builds
as current installation state. Do not commit local diagnostic captures, private
configuration, credentials or build output.

The release workflow packages a matching app/layer/helper and installer scripts
and requires the configured minisign secret to sign archives. It runs only for a
tag or explicit release dispatch; pushing source does not publish a new release.
The in-app updater requires signed matched-bundle metadata and retains a previous
app/layer pair for rollback. The source installer remains available for local builds.
