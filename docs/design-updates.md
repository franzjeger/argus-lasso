# In-app updates

Scope: this updater replaces the desktop binary and its integration only. It does
not update the Vulkan layer or root sensor helper. Use the [paired installer](installation.md)
for current overlay builds. No new release is implied by current source changes.

Argus-Lasso checks GitHub for a newer release and can replace its own binary,
so a user on the per-user install does not have to re-run the install script
after every release.

## How it works

1. **Check** — `GET /repos/franzjeger/argus-lasso/releases/latest`.
   The tag is compared to `CARGO_PKG_VERSION` by dotted numeric components;
   a suffix like `-rc1` parses as `0`, so a pre-release never outranks the
   release it precedes.
2. **Pick the asset** — the release publishes
   `argus-lasso-<version>-<arch>-linux.tar.gz` plus a `.sha256` and a
   `.minisig`. The arch comes from `std::env::consts::ARCH`, so an aarch64
   build never downloads the x86_64 tarball. A release with no signature is
   reported as available but cannot be self-installed.
3. **Verify** — the tarball is hashed against the published checksum, then
   its minisign signature is checked against the public key compiled into
   the binary. Either failure aborts before anything is written.
4. **Install** — the binary is extracted, written next to the running one as
   `.argus-lasso.update`, marked executable, and `rename(2)`d over the
   target. Rename is atomic and works on a running binary; writing over it
   in place fails with `ETXTBSY`. The desktop entry, systemd unit and icons
   are refreshed afterwards where they already exist.
5. **Restart** — the daemon is asked to restore everything it changed
   (nices, throttles, parked CPUs) and confirms before `exec(2)` replaces
   the process image, keeping the same PID. `exec` unwinds nothing, so
   skipping that handshake would strand parked CPUs offline with the
   original values gone. There is no window where neither the old nor the
   new process is running.

All network and disk work happens on a worker thread; the UI polls a channel
once per frame.

## Where it deliberately does nothing

**System-wide installs.** Before downloading, the updater probes whether the
directory holding the binary is writable by creating and removing a temp
file. A distro package or a `/usr/local` install is root-owned, so the probe
fails and the user is told to use their package manager instead. The probe
is a write test rather than a mode-bit check because mode bits miss ACLs and
read-only mounts.

## Signatures

The `.sha256` alone was never an authenticity check: it ships from the same
release as the tarball, so anyone who could replace one could replace the
other. It is kept because it distinguishes a truncated download from a
tampered one, which a signature failure alone does not — but the check that
establishes provenance is a detached **minisign** signature.

The release workflow signs each tarball with a key held in repository
secrets and publishes `<tarball>.minisig` beside it. The matching public key
is committed at `dist/argus-lasso.pub` and compiled into the binary with
`include_str!`, so verification does not depend on anything fetched at
update time.

Verification happens before a single byte is written: checksum, then
signature, then extraction. `allow_legacy` is set when verifying, which
selects minisign's non-prehashed mode rather than a weaker one — both are
Ed25519 over the same key, and accepting both keeps verification working
whichever minisign version the release runner has.

### Why it matters more here than for a normal download

A user who has installed the Gaming Mode helper also has a `NOPASSWD`
sudoers rule for a root-owned script. A self-updating binary on that machine
means whoever controls the release account controls a passwordless root path
— with no interaction beyond the user clicking "Update now". That is the
reason signing is a precondition for self-update rather than a refinement of
it.

### Bootstrapping the key

`dist/argus-lasso.pub` ships as a placeholder containing the marker
`NOT-YET-CONFIGURED`. A build carrying it **refuses to self-install** and
says so, rather than falling back to the checksum. Update checks still work,
so the user is told a new version exists and gets a link to it.

To configure it, generate a keypair and keep the secret key out of the
repository:

```bash
minisign -G -p dist/argus-lasso.pub -s ~/.minisign/argus-lasso.key
```

Then add two repository secrets — `MINISIGN_SECRET_KEY` (the contents of the
`.key` file) and `MINISIGN_PASSWORD` — and commit the regenerated
`dist/argus-lasso.pub`. The release job fails loudly if the secret is
missing, rather than publishing something clients will reject.

Sigstore/cosign keyless signing is the alternative worth revisiting if the
long-lived key in CI ever becomes the uncomfortable part: it binds the
signature to the workflow identity instead, at the cost of a much heavier
verification dependency.

## What the updater refreshes besides the binary

`make install` lays down a desktop entry, a systemd user unit and icons in
eight sizes. Replacing only the binary would freeze all of those at whatever
version first installed them, so a release that changes the icon or the
unit's flags would silently not take effect.

After the binary swap the updater rewrites, **only where the file already
exists**:

- `~/.local/share/applications/argus-lasso.desktop`, with `Exec=` pointed at
  the real binary path — the same substitution the Makefile makes.
- `~/.config/systemd/user/argus-lasso.service`, followed by
  `systemctl --user daemon-reload`, since a rewritten unit is otherwise
  inert.
- `~/.local/share/icons/hicolor/…`, re-rendered from the tiered vector
  masters in the tarball, matching the Makefile's tiering. Raster sizes are
  skipped when the host has neither `rsvg-convert` nor `magick`: a stale
  icon beats a blurry one.

Missing files are never created. Doing so would guess at a layout the user
may not have — a distro package, a different XDG root, or a deliberate
choice not to install a unit. All of this is best-effort and runs after the
rename, so a desktop file that could not be rewritten is a log line, not a
failed update.

## Trust roots

`ureq` is built with `rustls` and the bundled Mozilla root set, so
verification does not depend on the host's certificate store being sane. The
practical consequence is that a TLS-intercepting proxy will fail the check
with an `UnknownIssuer` error rather than silently trusting the interceptor —
which is the behaviour we want for a self-updater.
