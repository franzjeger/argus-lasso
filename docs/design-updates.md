# In-app updates

This describes the implementation in `src/updater.rs`, reviewed on 2026-09-28.
The updater replaces the desktop binary and selected existing integration files.
It does **not** update the Vulkan layer or privileged sensor helper. Use the
[paired installer](installation.md) for the current app/layer build.

The latest published release checked on 2026-09-28 is
[v1.3.1](https://github.com/franzjeger/argus-lasso/releases/tag/v1.3.1).
The source package version is also 1.3.1, although `master` contains subsequent
changes. “Up to date” compares release versions; it does not mean a source build
matches the latest commit. Pushing to `master` does not publish a release.

## Check, verify and install

1. Query `GET /repos/franzjeger/argus-lasso/releases/latest` and compare the tag
   with `CARGO_PKG_VERSION`. The comparator splits numeric components on `.`,
   `-` and `+`, treating non-numeric components as zero. This is not a complete
   Semantic Versioning parser; prerelease/build metadata ordering is limited.
2. Select the host architecture's `-<arch>-linux.tar.gz` asset and its checksum
   and signature. Missing signatures prevent self-installation. Downloads use
   HTTPS, a timeout and a size limit.
3. Verify SHA-256 and the detached minisign signature against the public key
   compiled into the binary from `dist/argus-lasso.pub`. A checksum detects a
   mismatch; the signature authenticates the archive to the configured key.
4. Extract exactly one matching application binary. Stage it beside the current
   executable as `.argus-lasso.update`, set executable permissions and run its
   `--version`. The reported version must be newer than the running package
   version; an old signed archive relabeled with a newer release tag is refused.
5. Atomically rename the staged binary over the installed one. Refresh existing
   support files on a best-effort basis. The current process keeps running its
   old executable until restarted.
6. On **Restart now**, ask the monitor to shut down and perform policy cleanup
   before replacing the process with `exec`. The wait is bounded to three seconds;
   on timeout it logs a warning and continues. This is not a guarantee that every
   policy was restored.

Network and installation work run on a worker thread. A write probe checks the
installation directory before download. An ordinary user cannot self-update a
root-owned system installation and is directed to the package manager.

## Signing configuration

`dist/argus-lasso.pub` contains a real minisign public key. The release workflow
requires `MINISIGN_SECRET_KEY` and `MINISIGN_PASSWORD`, signs each archive, and
verifies the signature against the checked-in public key before publishing it.
A missing secret fails the signing job. Secret values are not part of the source
and are not verified by this documentation review.

The updater retains a guard for custom builds containing `NOT-YET-CONFIGURED`;
that marker is **not** the current shipped key. Both supported minisign signature
modes are accepted by the verifier. There is no checksum-only fallback.

Privileged CPU controls use separate polkit-authorized helpers. The former
blanket `NOPASSWD` sudoers arrangement was removed in v1.2.0. The GUI and Vulkan
layer run as the normal user. The optional sensor service is installed separately.

## Integration files

After replacing the application, the updater refreshes these files only when
already present:

- `~/.local/share/applications/argus-lasso.desktop`, with the installed binary path.
- `~/.config/systemd/user/argus-lasso.service`, followed by a user daemon reload.
  This replaces the unit with the release's template, including its arguments;
  it does not preserve arbitrary local edits to that file.
- Existing Argus icons under `~/.local/share/icons/hicolor/`, using archive assets.
  Raster rendering requires `rsvg-convert` or `magick`.

It does not refresh the separate portal desktop entry, Vulkan manifest/libraries
or sensor service. This differs from the paired installer, which installs the
app/layer together, writes both desktop entries and preserves an existing user
service file. Support-file failures are logged after the binary replacement.

## Release workflow

The workflow builds x86_64 and aarch64 archives containing the app, Vulkan layer,
sensor helper, installers, documentation and assets. It runs on a version tag or
explicit workflow dispatch. Release notes come from the matching version section
in `CHANGELOG.md`; a missing section fails the notes job. These are the current
workflow's capabilities, not a claim that v1.3.1 archives contain newer features.
