# Installation

These instructions apply to the current source tree. Published v1.3.1 packages
contain the earlier desktop app; they do not contain the new overlay/recorder.
Do not mix daemon and layer files from arbitrary revisions.

## Dependencies

Build with Rust 1.92 or newer, Cargo, pkg-config, Wayland/X11 development libraries,
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
a versioned layer and manifest, and starts/restarts the user service. Close an
unmanaged manually launched Argus instance first; the installer refuses to run a
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

Future archives produced by this tree include the app, layer, optional sensor
reader, desktop files and installer scripts. After verifying the archive checksum
and minisign signature, extract it and run from the extracted directory:

```bash
./scripts/install-binaries.sh . "$(cat BUILD_ID)"
# Optional helper installation from the same archive:
./scripts/install-sensors.sh .
```

Older archives lack these files. They install only their original app. The in-app
updater currently replaces the GUI binary and existing desktop integration; it
does **not** upgrade the Vulkan layer or privileged sensor binary. Use the paired
installer for current overlay builds. No new release is implied by an Unreleased
changelog entry.

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
sudo systemctl daemon-reload
```

This does not remove the distinct CPU control helpers or your recorded data.
