# Installation

These instructions apply to the current source tree. As checked on 2026-09-28,
the latest published release is v1.3.1. Its packages
contain the earlier desktop app; they do not contain the new overlay/recorder.
Do not mix daemon and layer files from arbitrary revisions.

## Dependencies

Build with Rust 1.95 or newer, Cargo, pkg-config, Wayland/X11 development libraries,
an OpenGL driver for the GUI, and `glslangValidator` for the Vulkan shaders.
The source installer also uses Python 3 and systemd user services. A Vulkan loader
and working Vulkan driver are needed by games using the overlay.

Example development packages (distribution repositories may ship an older Rust;
check `rustc --version`):

```bash
# Arch Linux / CachyOS
sudo pacman -S --needed rust pkgconf wayland libxkbcommon libx11 libxi \
  libxcursor libxrandr glslang vulkan-icd-loader python desktop-file-utils librsvg

# Debian / Ubuntu
sudo apt install cargo pkg-config libwayland-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libxcb1-dev libx11-dev libxi-dev libxcursor-dev \
  libxrandr-dev libgl1-mesa-dev glslang-tools libvulkan1 python3 \
  desktop-file-utils librsvg2-bin
```

Optional: `polkit` and a desktop authentication agent for CPU control and extended
sensors; `kdialog`, `zenity` or `qarma` for file dialogs; `sqlite3` for Lutris;
a GlobalShortcuts portal backend for recording keys. NVIDIA telemetry uses NVML,
not repeated `nvidia-smi` subprocesses. MangoHud and vkcube are only needed for
the developer comparison script.

## User-local install

```bash
git clone https://github.com/franzjeger/argus-lasso.git
cd argus-lasso
make install
make enable  # optional autostart
```

`make install` builds a matching app/layer pair, installs icons, desktop entries,
a versioned layer and manifest, and starts/restarts the user service. Existing
service-file customizations and the manifest's automatic-loading choice are
preserved. Close an unmanaged manually launched Argus instance first; the installer refuses to run a
second daemon beside it. The supplied paired installer targets `~/.local`.

Installed locations:

| Item | Path |
|---|---|
| App | `~/.local/bin/argus-lasso` |
| Versioned layer | `~/.local/share/argus-lasso/layers/<build-id>/libargus_layer.so` |
| Vulkan manifest | `~/.local/share/vulkan/implicit_layer.d/ArgusOverlay.json` |
| User service | `~/.config/systemd/user/argus-lasso.service` |
| Desktop/portal entries | `~/.local/share/applications/` |
| Config | `~/.config/argus-lasso/config.toml` |

The installed startup log records the app/layer build ID and IPC version.
`argus-lasso --version` reports the Cargo package version, which source builds
between releases share with the last release; use the build ID to identify a
particular installation.
[Verified source and release status](status.md).

The sensor helper is installed separately with `scripts/install-sensors.sh` and
activated in Gaming → Sensors. See [sensor access](sensors.md). The CPU parking,
power-profile and renice helpers are set up from Gaming → CPU & performance;
they are separate from the sensor service.

## Build without installing

```bash
cargo build --release --workspace --locked
cargo metadata --no-deps --format-version 1  # reports target_directory
```

Cargo's target directory can be changed by `CARGO_TARGET_DIR` or Cargo config;
`target/release` is not a universal output path. On the development host it was
`~/.cache/cargo-target/release`. Use metadata or the supplied scripts to locate it.

The GUI can be started directly from the build directory. The layer additionally
needs an installed Vulkan manifest with its actual absolute library path.
`make reinstall` rebuilds and reinstalls the matched pair. Restart games after
replacing the layer; existing mapped libraries are retained in versioned directories.

## Binary archives

Release archives from 1.4.0 on contain the app, layer, optional sensor reader,
desktop files and installer scripts. Check the archive's checksum and its
signature against the release key ([`dist/argus-lasso.pub`](../dist/argus-lasso.pub))
before installing it, then install from the extracted directory:

```bash
version=1.4.0
pkg=argus-lasso-$version-$(uname -m)-linux
curl -fL --remote-name-all \
  https://github.com/franzjeger/argus-lasso/releases/download/v$version/$pkg.tar.gz{,.sha256,.minisig}
sha256sum -c "$pkg.tar.gz.sha256"
minisign -Vm "$pkg.tar.gz" -P RWREF/+QBa091Zu8cM6JWhgU7AoKI8LOqMfQcF9DbUvkK1QBuBUJh6g1
tar xzf "$pkg.tar.gz" && cd "$pkg"
./scripts/install-binaries.sh . "$(cat BUILD_ID)"
# Optional helper installation from the same archive:
./scripts/install-sensors.sh .
```

This is also how 1.3.1 and older move to 1.4.0, once: their updater checks
against the release key that was replaced on 2026-09-30 and refuses newer
archives. From 1.4.0 on, the in-app updater installs a matched app/layer pair
from signed archives containing bundle metadata and retains a rollback. The
privileged sensor helper remains separate. Archives of 1.3.1 and older contain
only the desktop app. See [updater behavior](design-updates.md).

`dist/PKGBUILD` is an older-release packaging template, not evidence of a published
AUR package or a complete current overlay package. See its header before use.

## Remove or disable

`make disable` stops/disables the Argus user service. `make uninstall` removes its
user-local app, desktop entries, icons, manifest and versioned layers; configuration
and recordings remain. Close games first if removing libraries they currently use.

The system sensor service is a separate optional installation. To remove it:

```bash
sudo systemctl disable --now argus-sensors.service
sudo rm -f /etc/systemd/system/argus-sensors.service /usr/local/libexec/argus-sensors
sudo rm -rf /etc/systemd/system/argus-sensors.service.d
sudo systemctl daemon-reload
```

This does not remove the distinct CPU control helpers or your recorded data.
`make uninstall` prints the commands for the helpers when they are installed:

```bash
sudo rm -rf /usr/local/lib/argus-lasso
sudo rm -f /usr/share/polkit-1/actions/io.github.franzjeger.argus-lasso.policy
sudo rm -f /etc/polkit-1/rules.d/50-argus-lasso.rules
```

The helpers' polkit actions ask for an administrator's password by default. The
polkit rule installed with them lets the user who installed them run them without
a password from an active local session; other local users are asked.
