# Security Policy

## Supported Versions

Only the latest version of this project is currently supported with security
updates. The [project status](docs/status.md) distinguishes the latest published
release from subsequent development commits; the package version alone does not
identify a source build.

## Reporting a Vulnerability

If you discover a security vulnerability within this project, please open an
issue or contact the repository owner directly. Include the release tag or
source commit and affected component (desktop app, Vulkan layer, updater or
privileged helper). Do not include credentials or private configuration in
public reports. We will investigate all legitimate reports and do our best to
quickly resolve the issue.

## Security model

What runs with which authority, so a report can say which boundary it crosses:

- **The app and the Vulkan layer run as your user**, never as root. The layer is
  loaded into games and talks to the app over a Unix socket in
  `$XDG_RUNTIME_DIR/argus-lasso/`.
- **Root is reached only through three small helpers** (CPU parking, power
  profile, process priority), each authorised separately by polkit. They take a
  fixed set of validated arguments; priority changes are restricted to the
  calling user's own processes. Installing or updating them asks for
  authentication. The optional sensor service is a separate systemd unit whose
  output is readable by its group only.
- **Requests to a running instance** (`toggle-overlay`, `gaming`, `probalance`,
  `profile`, and a second launch asking for the window) are files in
  `~/.config/argus-lasso/`, in directories created with mode 0700. Anyone who
  can write there can already edit `config.toml`, so they grant nothing new. A
  request holds one line from a fixed vocabulary; anything else is discarded,
  and requests left while no instance ran are discarded at start.
- **Updates** are downloaded over HTTPS from this repository's releases and
  installed only if their minisign signature verifies against the public key
  built into the running app (`dist/argus-lasso.pub`). The signing key exists
  only in the `release` environment of this repository, and every release run
  waits for the maintainer's approval before it can sign.
- **Process actions in the GUI** (kill, suspend, priority, affinity) identify
  their target by PID and start time and signal through a pidfd, so a recycled
  PID is never acted on. The CLI's `kill` refuses PID 0, PID 1, its own PID and
  values that do not fit a `pid_t`.

Out of scope: what another process running as the same user can do to Argus;
it can already change the configuration and the user's own processes.

