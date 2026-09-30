# In-app updates

The updater installs a matching desktop app and Vulkan layer from a signed
GitHub release. The privileged sensor helper remains a separate installation.
A system-wide or distro-managed installation is left to the package manager.

## Check, verify and install

1. Query the latest stable GitHub release and compare its tag with
   `CARGO_PKG_VERSION` using Semantic Versioning precedence. Build metadata does
   not count as an upgrade. This checks releases, not development commits.
2. Download the host architecture's archive, SHA-256 checksum and minisign
   signature over HTTPS with a timeout and size limit. Unsigned releases cannot
   be installed by the updater.
3. Verify the checksum and signature against the key compiled from
   `dist/argus-lasso.pub`. No checksum-only fallback exists.
4. Extract exactly one app, layer and `bundle.json`. The signed metadata contains
   schema, version, architecture, build ID, IPC version and both binary hashes.
   Reject a missing member, mismatch or unsupported metadata schema before
   replacing any live files. Older archives without bundle metadata require
   manual installation.
5. Stage the app and verify its `--version` and `build-info` output against the
   release metadata. The version must be newer than the running package.
6. Write the layer to a new, unique directory. Existing mapped libraries are
   never overwritten. Preserve the overlay manifest's loading preferences.
7. Save the previous app and manifest, and fsync a pending recovery record before
   switching the manifest and app. These are two separate atomic file replacements,
   not a filesystem-wide atomic transaction. A failure triggers restoration of
   the previous pair; a pending record is recovered on the next startup of this
   updater. Clear the pending flag only after both replacements are durable.
8. Refresh existing desktop/portal entries and icons on a best-effort basis.
   Preserve local systemd user service customizations.

Network, archive and installation work run on a worker thread. Configuration
writes and manifest replacements use unique staging files and sync both file and
parent directory. Overlay loading changes are excluded while an update owns the
manifest; the UI reports that it should be retried when the update finishes.

## Restart and rollback

Restart Argus to run the new app. Restart games to load the matching layer;
existing game processes retain their mapped library. The IPC compatibility check
continues to reject incompatible daemon/layer protocols during this transition.

**Settings → Startup & updates → Restore previous app and overlay** restores the
last retained pair. Restart afterward. Alternatively, close Argus and run:

```bash
argus-lasso rollback-update
```

Rollback validates the backup binary's checksum and retains its recovery record
until both restores succeed. It restores the prior manifest, or removes the
manifest if none existed before installation. It does not downgrade configuration
files, restore desktop/icon artwork, or replace the privileged sensor service.
Layer directories nothing refers to any more are removed; the live one and the one
rollback would restore are kept. A game that still has a removed library mapped
keeps using it until it exits.

Before a restart, pending GUI termination actions are cancelled, settings saves
are queued ahead of shutdown, and the monitor is asked to restore scheduling
policy. Cleanup waits are bounded; a successful update does not prove every
external scheduling operation was restored.

## Release packaging and signing

The release workflow builds the whole workspace with one `ARGUS_BUILD_ID` and
packages the app, layer, sensor helper, installers, documentation and assets.
`scripts/package-bundle.py` obtains the app identity through `build-info`, verifies
that it matches the release tag, and writes `bundle.json` with the binary hashes.
The metadata is authenticated as part of the signed archive.

The workflow requires `MINISIGN_SECRET_KEY` and `MINISIGN_PASSWORD`, signs each
archive and verifies the signature against the checked-in public key before
publishing. Missing signing configuration fails the job.

The signing job runs in the `release` environment. It waits for the maintainer's
approval, can only be deployed from `v*` tags and `master` (for the manual
trigger), and holds the two signing values as environment secrets. The values
must not also be repository secrets: any workflow in the repository, including
one pushed to a new branch, can read those, and pushing a tag alone would then
be enough to get a release signed. This protects against leaked tokens and
collaborator access; it does not protect against someone who has taken over the
maintainer's own account, which only offline signing would.

The actions the workflows use are pinned by commit SHA (a tag can be moved to
other code; CI fails on an unpinned one), and the signing job builds without a
restored cache, which would be build output from another workflow run.

The key was replaced on 2026-09-30 (key ID `D53DAD0590FF1744`, previously
`4CF0D660D5D39564`), when the signing values moved to the environment: the old
secret key existed only as a repository secret, which GitHub never hands back,
and every workflow in the repository had been able to read it. Installed builds
check updates against the key compiled into them, so 1.3.1 and older refuse
releases signed with the new key and have to be updated by hand once. The
maintainer keeps the secret key and its password offline. Release notes come from
the matching version section in `CHANGELOG.md`. Pushing source does not publish a
release, and implementing this updater does not retrofit older published archives.
