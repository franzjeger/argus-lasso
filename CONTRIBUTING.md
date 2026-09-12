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

CI also checks Rust 1.92 and builds/tests on x86_64 and aarch64. An architecture
build is not proof of game/driver compatibility. Shader compilation requires
`glslangValidator` (Debian/Ubuntu package `glslang-tools`).

For screenshots, run `argus-lasso --ui-tour <output-directory>`. This isolated
preview uses read-only collection: it must never enforce policies, bind the
production overlay socket or save configuration. Root captures do not include
native child windows; photograph those separately. Review images for private
information before publishing. `examples/overlay-settings-preview.rs` renders the
actual customization controls without a daemon or configuration writes.

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
Keep dated investigation notes in `docs/archive/`; do not present old test builds
as current installation state. Do not commit local diagnostic captures, private
configuration, credentials or build output.

The release workflow packages a matching app/layer/helper and installer scripts
and requires the configured minisign secret to sign archives. It runs only for a
tag or explicit release dispatch; pushing source does not publish a new release.
The in-app updater still updates only the desktop binary and integration, so
current overlay installations must use the paired installer.
