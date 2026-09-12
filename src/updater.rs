//! In-app update check and self-install against the project's GitHub releases.
//!
//! The app replaces its own binary in place. That only works for the
//! per-user install (`~/.local/bin/argus-lasso`, what `make install` and the
//! release tarball produce); a system-wide or distro-packaged install is
//! owned by root and is left to the package manager, which this module
//! detects and reports rather than failing halfway through.
//!
//! Integrity: the release publishes a `.sha256` beside each tarball and the
//! download is checked against it. That catches a truncated or corrupted
//! transfer, not a compromised release — both files come from the same
//! place. Real tamper-resistance needs a detached signature over the tarball
//! with a key that does not live in the release; see the note in
//! `docs/design-updates.md`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

const REPO: &str = "franzjeger/argus-lasso";
const USER_AGENT: &str = concat!("argus-lasso/", env!("CARGO_PKG_VERSION"));
/// Releases are small (single-digit MB); anything larger is not ours.
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;
const NET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The version this binary was built as.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A release newer than the running binary, with the assets for this arch.
#[derive(Debug, Clone)]
pub struct Update {
    pub tag: String,
    pub version: String,
    pub page_url: String,
    tarball_url: String,
    sha256_url: String,
    /// `None` on releases published before signing existed.
    signature_url: Option<String>,
}

/// Progress and results from the background worker.
#[derive(Debug)]
pub enum Status {
    /// Already on the newest release.
    UpToDate,
    /// A newer release exists.
    Available(Box<Update>),
    /// Install finished; the new binary is in place and needs a restart.
    Installed,
    Error(String),
}

/// Handle to a background check or install.
pub struct Job {
    rx: Receiver<Status>,
}

impl Job {
    /// Non-blocking poll. Returns `None` while the work is still running.
    pub fn poll(&self) -> Option<Status> {
        use std::sync::mpsc::TryRecvError;
        match self.rx.try_recv() {
            Ok(status) => Some(status),
            Err(TryRecvError::Empty) => None,
            // The worker died without sending. Reporting it as an error is
            // what clears `busy`; treating it as "still running" would wedge
            // the UI, since both entry points refuse to start while busy.
            Err(TryRecvError::Disconnected) => Some(Status::Error(
                "the update worker stopped unexpectedly — try again".into(),
            )),
        }
    }
}

/// Start a check for a newer release.
pub fn check() -> Job {
    spawn(|tx| {
        let result = match latest_release() {
            Ok(Some(u)) => Status::Available(Box::new(u)),
            Ok(None) => Status::UpToDate,
            Err(e) => Status::Error(e),
        };
        let _ = tx.send(result);
    })
}

/// Start downloading and installing `update` over the running binary.
pub fn install(update: Update) -> Job {
    spawn(move |tx| {
        let result = match install_blocking(&update) {
            Ok(()) => Status::Installed,
            Err(e) => Status::Error(e),
        };
        let _ = tx.send(result);
    })
}

fn spawn(f: impl FnOnce(Sender<Status>) + Send + 'static) -> Job {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || f(tx));
    Job { rx }
}

// ── Release lookup ────────────────────────────────────────────────────────

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(NET_TIMEOUT))
        .user_agent(USER_AGENT)
        .build()
        .into()
}

/// Fetch the latest release and return it if it is newer than this build.
fn latest_release() -> Result<Option<Update>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("could not read the response: {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("unexpected response from GitHub: {e}"))?;

    let tag = json["tag_name"]
        .as_str()
        .ok_or("release has no tag_name")?
        .to_string();
    let version = tag.trim_start_matches('v').to_string();

    if !is_newer(&version, current_version()) {
        return Ok(None);
    }

    // Assets are named argus-lasso-<version>-<arch>-linux.tar.gz
    let arch = std::env::consts::ARCH; // "x86_64" | "aarch64"
    let want = format!("-{arch}-linux.tar.gz");
    let assets = json["assets"].as_array().ok_or("release has no assets")?;

    let find = |suffix: &str| -> Option<String> {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            if name.ends_with(suffix) {
                a["browser_download_url"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
    };

    let tarball_url =
        find(&want).ok_or_else(|| format!("release {tag} has no build for {arch}"))?;
    let sha256_url = find(&format!("{want}.sha256"))
        .ok_or_else(|| format!("release {tag} has no checksum for {arch}"))?;
    // Absent on releases cut before signing existed. Missing it is not an
    // error here: the user should still learn a new version is out and get a
    // link to it. Only the self-install refuses, in install_blocking.
    let signature_url = find(&format!("{want}.minisig"));

    Ok(Some(Update {
        tag,
        version,
        page_url: json["html_url"].as_str().unwrap_or_default().to_string(),
        tarball_url,
        sha256_url,
        signature_url,
    }))
}

// ── Signature ─────────────────────────────────────────────────────────────

/// The minisign public key releases are signed with. Public by definition;
/// the matching secret key exists only in the release workflow's secret
/// store, which is what makes the signature mean more than the checksum.
const PUBLIC_KEY: &str = include_str!("../dist/argus-lasso.pub");

/// Marker in the committed placeholder key. A build that still carries it
/// cannot verify anything, so it refuses to self-install rather than falling
/// back to the checksum — which anyone who could swap the tarball could
/// recompute.
const PUBLIC_KEY_PLACEHOLDER: &str = "NOT-YET-CONFIGURED";

/// Verify `tarball` against `signature_text` using `public_key_text`.
///
/// `allow_legacy` is true because it selects minisign's non-prehashed mode,
/// not a weaker one: both variants are Ed25519 over the same key, the
/// prehashed form exists for streaming. Accepting both keeps verification
/// working whichever minisign version the release runner has.
fn verify_signature(
    public_key_text: &str,
    tarball: &[u8],
    signature_text: &str,
) -> Result<(), String> {
    use minisign_verify::{PublicKey, Signature};

    if public_key_text.contains(PUBLIC_KEY_PLACEHOLDER) {
        return Err("this build has no release signing key compiled in, so the \
                    download cannot be verified. Install the update manually."
            .into());
    }
    let key = PublicKey::decode(public_key_text.trim())
        .map_err(|e| format!("the built-in signing key is unusable: {e}"))?;
    let signature = Signature::decode(signature_text.trim())
        .map_err(|e| format!("the release signature is malformed: {e}"))?;
    key.verify(tarball, &signature, true)
        .map_err(|e| format!("signature check failed: {e}. The download was not installed."))
}

/// Compare dotted numeric versions. Non-numeric parts sort as 0, so a
/// pre-release suffix never reads as newer than the release it precedes.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parts(candidate), parts(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

// ── Install ───────────────────────────────────────────────────────────────

/// Where this process's own binary lives, resolved through any symlink.
fn install_target() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("could not find the running binary: {e}"))?;
    // Once the updater replaces the binary via rename, the kernel marks the
    // running image as "(deleted)", so current_exe() returns a path with that
    // suffix and canonicalize() fails with ENOENT. Strip the suffix: the new
    // binary lives at the real path, which is what we want to exec.
    let exe = strip_deleted_suffix(&exe);
    Ok(std::fs::canonicalize(&exe).unwrap_or(exe))
}

/// The kernel appends ` (deleted)` to `/proc/self/exe` after the running image
/// is replaced (rename over it). Strip that suffix so the path points at the
/// new binary rather than the deleted one.
fn strip_deleted_suffix(path: &Path) -> PathBuf {
    const SUFFIX: &str = " (deleted)";
    match path.to_string_lossy().strip_suffix(SUFFIX) {
        Some(stripped) => PathBuf::from(stripped),
        None => path.to_path_buf(),
    }
}

/// True when we can replace `path` — i.e. its directory is writable by us.
fn can_write(path: &Path) -> bool {
    let dir = match path.parent() {
        Some(d) => d,
        None => return false,
    };
    // Probe by creating and removing a temp file; a read-only check on the
    // mode bits would miss ACLs, read-only mounts and root-owned dirs.
    let probe = dir.join(".argus-lasso-write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn install_blocking(update: &Update) -> Result<(), String> {
    let target = install_target()?;
    if !can_write(&target) {
        return Err(format!(
            "{} is not writable by this user — it looks like a system-wide \
             install, so update it with your package manager instead.",
            target.display()
        ));
    }

    // Refuse before spending a download on a release we could not trust.
    let Some(signature_url) = update.signature_url.as_deref() else {
        return Err(format!(
            "release {} is not signed, so it cannot be verified. \
             Download it manually from {}",
            update.tag, update.page_url
        ));
    };

    let tarball = fetch(&update.tarball_url)?;

    // Checksum first: it is the same cost and tells a truncated download
    // apart from a tampered one, which a signature failure alone would not.
    // The checksum file is "<hex>  <filename>"; take the first field.
    let sha_text = String::from_utf8(fetch(&update.sha256_url)?)
        .map_err(|_| "checksum file is not valid text".to_string())?;
    let expected = sha_text
        .split_whitespace()
        .next()
        .ok_or("checksum file is empty")?
        .to_lowercase();
    let actual = sha256_hex(&tarball);
    if actual != expected {
        return Err(format!(
            "checksum mismatch — expected {expected}, got {actual}. \
             The download was not installed."
        ));
    }

    // Then the signature, which is the check that actually establishes the
    // tarball came from whoever holds the release key.
    let signature_text = String::from_utf8(fetch(signature_url)?)
        .map_err(|_| "signature file is not valid text".to_string())?;
    verify_signature(PUBLIC_KEY, &tarball, &signature_text)?;

    let binary = extract_binary(&tarball)?;

    // Write beside the target and rename: rename is atomic, and it works
    // even though the old binary is running, whereas writing over it in
    // place fails with ETXTBSY.
    let dir = target.parent().ok_or("binary has no parent directory")?;
    let staged = dir.join(".argus-lasso.update");
    std::fs::write(&staged, &binary).map_err(|e| format!("could not stage the update: {e}"))?;
    set_executable(&staged)?;
    std::fs::rename(&staged, &target).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("could not replace the binary: {e}")
    })?;

    // Best-effort, and deliberately after the rename: the update has already
    // succeeded by this point, so a desktop file we could not rewrite is a
    // log line, not a failed install.
    for note in refresh_support_files(&tarball, &target) {
        log::info!("update: {note}");
    }
    Ok(())
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let mut resp = agent()
        .get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;

    // A `take(MAX)` that silently truncates an oversized body would surface
    // later as a confusing "checksum mismatch". Detect the cap up front
    // (Content-Length) and while reading, and fail with a clear message.
    if let Some(len) = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        if len > MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "download is {len} bytes, exceeds the {MAX_DOWNLOAD_BYTES}-byte limit"
            ));
        }
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut reader = resp.body_mut().as_reader();
    let mut chunk = [0u8; 8192];
    loop {
        if buf.len() as u64 >= MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "download exceeds the {MAX_DOWNLOAD_BYTES}-byte limit"
            ));
        }
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) => return Err(format!("download failed: {e}")),
        }
    }
    if buf.is_empty() {
        return Err("download was empty".into());
    }
    Ok(buf)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Release tarballs unpack to a single `argus-lasso-<version>-<arch>-linux/`
/// directory, so the binary always sits exactly one level down.
///
/// `--no-wildcards-match-slash` matters: tar's `*` spans `/` by default, so a
/// bare `*/argus-lasso` would match at any depth. Pinning it to one level,
/// plus the member count check in `extract_binary`, keeps a stray second
/// match from being concatenated onto the real binary.
const BINARY_MEMBER: &str = "*/argus-lasso";
const TAR_MATCH_FLAGS: [&str; 2] = ["--wildcards", "--no-wildcards-match-slash"];

/// Run `tar` with the tarball on stdin and return its stdout.
///
/// Shells out rather than taking a tar+gzip dependency: tar is present on
/// every target system, and the archive is our own.
fn run_tar(args: &[&str], tarball: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("tar")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run tar: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("tar refused stdin")?;
    let data = tarball.to_vec();
    // Feed tar from a thread: a release tarball is larger than the pipe
    // buffer, so writing inline would deadlock against our own read.
    let writer = std::thread::spawn(move || stdin.write_all(&data));

    let out = child
        .wait_with_output()
        .map_err(|e| format!("tar failed: {e}"))?;
    let _ = writer.join();

    if !out.status.success() {
        return Err(format!(
            "could not unpack the release: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Pull the `argus-lasso` binary out of the release tarball.
fn extract_binary(tarball: &[u8]) -> Result<Vec<u8>, String> {
    // List first: `tar -xO` concatenates every match into one stream, so an
    // archive carrying two `*/argus-lasso` entries would yield a spliced file
    // that we would then mark executable. Refuse anything but a single match.
    let listing = run_tar(
        &["-tz", TAR_MATCH_FLAGS[0], TAR_MATCH_FLAGS[1], BINARY_MEMBER],
        tarball,
    )?;
    let matches = String::from_utf8_lossy(&listing)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    match matches {
        0 => return Err("the release archive contained no argus-lasso binary".into()),
        1 => {}
        n => {
            return Err(format!(
                "the release archive contained {n} argus-lasso entries; refusing to install"
            ))
        }
    }

    let binary = run_tar(
        &[
            "-xzO",
            TAR_MATCH_FLAGS[0],
            TAR_MATCH_FLAGS[1],
            BINARY_MEMBER,
        ],
        tarball,
    )?;
    if binary.is_empty() {
        return Err("the release archive contained no argus-lasso binary".into());
    }
    Ok(binary)
}

// ── Support files ─────────────────────────────────────────────────────────

/// Pull one member out of the tarball, or `None` if it is not there.
fn extract_member(tarball: &[u8], member: &str) -> Option<Vec<u8>> {
    let out = run_tar(
        &["-xzO", TAR_MATCH_FLAGS[0], TAR_MATCH_FLAGS[1], member],
        tarball,
    )
    .ok()?;
    (!out.is_empty()).then_some(out)
}

/// Rewrite `path` with `contents` when it already exists and differs.
/// Returns a note for the log when something changed.
fn refresh_if_present(path: &Path, contents: &[u8], label: &str) -> Option<String> {
    if !path.exists() {
        return None;
    }
    if std::fs::read(path).is_ok_and(|old| old == contents) {
        return None;
    }
    match std::fs::write(path, contents) {
        Ok(()) => Some(format!("refreshed {label}")),
        Err(e) => Some(format!("could not refresh {label}: {e}")),
    }
}

/// Refresh the user-local files `make install` puts alongside the binary.
///
/// Swapping the binary alone leaves the desktop entry, the systemd user unit
/// and the icons frozen at whatever version first installed them, so a
/// release that changes any of them silently does not take effect.
///
/// Only files that already exist are rewritten. Creating missing ones would
/// guess at a layout the user may not have — a distro package, a different
/// XDG root, or a deliberate choice not to install a unit at all.
fn refresh_support_files(tarball: &[u8], target: &Path) -> Vec<String> {
    let Ok(home) = std::env::var("HOME") else {
        return vec!["HOME is unset; left the desktop and icon files alone".into()];
    };
    let home = PathBuf::from(home);
    let exe = target.display().to_string();
    let mut notes = Vec::new();

    // .desktop — Exec= is rewritten to the real binary path, the same
    // substitution the Makefile does at install time.
    if let Some(raw) = extract_member(tarball, "*/dist/argus-lasso.desktop") {
        if let Ok(text) = String::from_utf8(raw) {
            let patched = text.replace("Exec=argus-lasso", &format!("Exec={exe}"));
            notes.extend(refresh_if_present(
                &home.join(".local/share/applications/argus-lasso.desktop"),
                patched.as_bytes(),
                "desktop entry",
            ));
        }
    }

    // systemd user unit — ExecStart carries an absolute path plus flags, so
    // swap only the path and keep whatever arguments the release ships.
    if let Some(raw) = extract_member(tarball, "*/dist/argus-lasso.service") {
        if let Ok(text) = String::from_utf8(raw) {
            let patched = text.replace("%h/.local/bin/argus-lasso", &exe);
            let unit = home.join(".config/systemd/user/argus-lasso.service");
            if let Some(note) = refresh_if_present(&unit, patched.as_bytes(), "systemd user unit") {
                notes.push(note);
                // A rewritten unit is inert until systemd re-reads it.
                let _ = std::process::Command::new("systemctl")
                    .args(["--user", "daemon-reload"])
                    .output();
            }
        }
    }

    notes.extend(refresh_icons(tarball, &home));
    notes
}

/// Refresh installed icons from the tarball's vector masters.
///
/// Mirrors the Makefile's tiering: below 48px the full artwork turns to
/// mush, so small sizes render from their own master. Only sizes already
/// present are touched, and PNG sizes are skipped entirely when the host has
/// no renderer — a stale icon beats a blurry one.
fn refresh_icons(tarball: &[u8], home: &Path) -> Vec<String> {
    let hicolor = home.join(".local/share/icons/hicolor");
    if !hicolor.is_dir() {
        return Vec::new();
    }
    let mut notes = Vec::new();

    // The scalable master is a plain copy — no renderer needed.
    if let Some(svg) = extract_member(tarball, "*/assets/icon.svg") {
        notes.extend(refresh_if_present(
            &hicolor.join("scalable/apps/argus-lasso.svg"),
            &svg,
            "scalable icon",
        ));
    }

    let renderer = ["rsvg-convert", "magick"]
        .into_iter()
        .find(|bin| which(bin));
    let Some(renderer) = renderer else {
        notes.push("no rsvg-convert or magick; left the raster icons alone".into());
        return notes;
    };

    let masters = [
        ("*/assets/icon-small.svg", [16, 22, 24, 32].as_slice()),
        ("*/assets/icon-medium.svg", [48].as_slice()),
        ("*/assets/icon.svg", [64, 128, 256].as_slice()),
    ];
    let tmp = std::env::temp_dir().join(format!("argus-icon-{}.svg", std::process::id()));
    for (member, sizes) in masters {
        let Some(svg) = extract_member(tarball, member) else {
            continue;
        };
        if std::fs::write(&tmp, &svg).is_err() {
            continue;
        }
        for &size in sizes {
            let dest = hicolor.join(format!("{size}x{size}/apps/argus-lasso.png"));
            if !dest.exists() {
                continue;
            }
            if render_icon(renderer, &tmp, size, &dest) {
                notes.push(format!("refreshed {size}px icon"));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    notes
}

fn which(bin: &str) -> bool {
    std::process::Command::new("which")
        .arg(bin)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn render_icon(renderer: &str, svg: &Path, size: u32, dest: &Path) -> bool {
    let n = size.to_string();
    let mut cmd = std::process::Command::new(renderer);
    if renderer == "rsvg-convert" {
        cmd.args(["-w", &n, "-h", &n]).arg(svg).arg("-o").arg(dest);
    } else {
        cmd.args(["-background", "none"])
            .arg(svg)
            .args(["-resize", &format!("{n}x{n}")])
            .arg(dest);
    }
    cmd.output().is_ok_and(|o| o.status.success())
}

fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("could not mark the update executable: {e}"))
}

/// Replace this process with the freshly installed binary.
///
/// `exec` keeps the same PID and never returns on success, so there is no
/// window where neither the old nor the new process is running.
///
/// It also means nothing unwinds: no destructors, and eframe's `on_exit`
/// never runs. The caller MUST have driven the daemon shutdown first (see
/// `monitor::shutdown_and_wait`), or parked CPUs stay offline and throttled
/// processes keep their raised nice — the originals live only in this
/// process's memory and are gone the moment the image is replaced. Callers
/// set `UpdateState::restart_requested` rather than calling this directly, so
/// that ordering lives in exactly one place.
pub fn restart() -> String {
    use std::os::unix::process::CommandExt;
    let exe = match install_target() {
        Ok(p) => p,
        Err(e) => return e,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    format!(
        "could not restart: {}",
        std::process::Command::new(exe).args(args).exec()
    )
}

#[cfg(test)]
mod tests {
    use super::{extract_binary, is_newer, sha256_hex, strip_deleted_suffix, verify_signature};

    #[test]
    fn strips_kernel_deleted_suffix() {
        let p = std::path::Path::new("/home/u/.local/bin/argus-lasso (deleted)");
        assert_eq!(
            strip_deleted_suffix(p),
            std::path::PathBuf::from("/home/u/.local/bin/argus-lasso")
        );
    }

    #[test]
    fn leaves_normal_path_untouched() {
        let p = std::path::Path::new("/home/u/.local/bin/argus-lasso");
        assert_eq!(
            strip_deleted_suffix(p),
            std::path::PathBuf::from("/home/u/.local/bin/argus-lasso")
        );
    }

    #[test]
    fn newer_patch_minor_and_major() {
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
    }

    #[test]
    fn same_or_older_is_not_newer() {
        assert!(!is_newer("1.0.9", "1.0.9"));
        assert!(!is_newer("1.0.8", "1.0.9"));
        assert!(!is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn missing_components_count_as_zero() {
        assert!(is_newer("1.1", "1.0.9"));
        assert!(!is_newer("1.0", "1.0.0"));
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        // Empty input, from FIPS 180-4.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// Build a release-shaped tarball and pull the binary back out, which is
    /// the step most likely to break if the archive layout ever changes.
    #[test]
    fn extract_binary_finds_the_payload() {
        let dir = std::env::temp_dir().join(format!("argus-extract-{}", std::process::id()));
        let pkg = dir.join("argus-lasso-9.9.9-x86_64-linux");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("argus-lasso"), b"#!/bin/sh\necho hi\n").unwrap();
        std::fs::write(pkg.join("README.md"), b"not the binary").unwrap();

        let out = std::process::Command::new("tar")
            .arg("-czf")
            .arg(dir.join("release.tar.gz"))
            .arg("-C")
            .arg(&dir)
            .arg("argus-lasso-9.9.9-x86_64-linux")
            .output()
            .unwrap();
        assert!(out.status.success(), "tar -czf failed");

        let tarball = std::fs::read(dir.join("release.tar.gz")).unwrap();
        let binary = extract_binary(&tarball).expect("extract");
        assert_eq!(binary, b"#!/bin/sh\necho hi\n");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_binary_rejects_an_archive_without_it() {
        let dir = std::env::temp_dir().join(format!("argus-empty-{}", std::process::id()));
        let pkg = dir.join("some-other-package");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("README.md"), b"nothing useful").unwrap();
        let out = std::process::Command::new("tar")
            .arg("-czf")
            .arg(dir.join("r.tar.gz"))
            .arg("-C")
            .arg(&dir)
            .arg("some-other-package")
            .output()
            .unwrap();
        assert!(out.status.success());
        let tarball = std::fs::read(dir.join("r.tar.gz")).unwrap();
        assert!(extract_binary(&tarball).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `tar -xO` concatenates every match, so two entries would splice into
    /// one file that we would mark executable. It must be refused instead.
    #[test]
    fn extract_binary_rejects_two_matching_entries() {
        let dir = std::env::temp_dir().join(format!("argus-dup-{}", std::process::id()));
        for pkg in [
            "argus-lasso-9.9.9-x86_64-linux",
            "argus-lasso-9.9.8-x86_64-linux",
        ] {
            let p = dir.join(pkg);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("argus-lasso"), b"payload").unwrap();
        }
        let out = std::process::Command::new("tar")
            .arg("-czf")
            .arg(dir.join("r.tar.gz"))
            .arg("-C")
            .arg(&dir)
            .arg("argus-lasso-9.9.9-x86_64-linux")
            .arg("argus-lasso-9.9.8-x86_64-linux")
            .output()
            .unwrap();
        assert!(out.status.success(), "tar -czf failed");

        let tarball = std::fs::read(dir.join("r.tar.gz")).unwrap();
        let err = extract_binary(&tarball).expect_err("two entries must be refused");
        assert!(err.contains('2'), "error should name the count: {err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A nested path must not match: tar's `*` spans `/` unless told not to,
    /// and a deep entry is not the release layout we publish.
    #[test]
    fn extract_binary_ignores_a_nested_match() {
        let dir = std::env::temp_dir().join(format!("argus-nested-{}", std::process::id()));
        let deep = dir.join("argus-lasso-9.9.9-x86_64-linux").join("dist");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("argus-lasso"), b"not the real one").unwrap();
        let out = std::process::Command::new("tar")
            .arg("-czf")
            .arg(dir.join("r.tar.gz"))
            .arg("-C")
            .arg(&dir)
            .arg("argus-lasso-9.9.9-x86_64-linux")
            .output()
            .unwrap();
        assert!(out.status.success(), "tar -czf failed");

        let tarball = std::fs::read(dir.join("r.tar.gz")).unwrap();
        assert!(extract_binary(&tarball).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    // A published minisign test vector, so the wrapper is exercised without a
    // key of our own. Payload is the four bytes "test".
    const TEST_PUB: &str = "untrusted comment: minisign public key E7620F1842B4E81F
RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const TEST_SIG: &str = "untrusted comment: signature from minisign secret key
RWQf6LRCGA9i59SLOFxz6NxvASXDJeRtuZykwQepbDEGt87ig1BNpWaVWuNrm73YiIiJbq71Wi+dP9eKL8OC351vwIasSSbXxwA=
trusted comment: timestamp:1555779966\tfile:test
QtKMXWyYcwdpZAlPF7tE2ENJkRd1ujvKjlj1m9RtHTBnZPa5WKU5uWRs5GoP5M/VqE81QFuMKI5k/SfNQUaOAA==";

    #[test]
    fn signature_accepts_the_payload_it_covers() {
        verify_signature(TEST_PUB, b"test", TEST_SIG).expect("valid signature must verify");
    }

    #[test]
    fn signature_rejects_a_modified_payload() {
        let err = verify_signature(TEST_PUB, b"Test", TEST_SIG)
            .expect_err("a changed payload must not verify");
        assert!(err.contains("not installed"), "unhelpful message: {err}");
    }

    /// The committed placeholder must fail closed. Falling back to the
    /// checksum would be worse than useless — anyone able to swap the
    /// tarball could recompute it.
    #[test]
    fn unconfigured_key_refuses_to_verify() {
        let placeholder = super::PUBLIC_KEY_PLACEHOLDER;
        let err = verify_signature(
            &format!("untrusted comment: x\n{placeholder}\n"),
            b"test",
            TEST_SIG,
        )
        .expect_err("placeholder key must not verify anything");
        assert!(err.contains("no release signing key"), "wrong error: {err}");
    }

    /// Guards the bootstrap state: until a real key is committed, the shipped
    /// build must be the refusing one rather than silently trusting.
    #[test]
    fn shipped_key_is_either_real_or_recognisably_absent() {
        let key = super::PUBLIC_KEY;
        if key.contains(super::PUBLIC_KEY_PLACEHOLDER) {
            assert!(
                verify_signature(key, b"test", TEST_SIG).is_err(),
                "placeholder key must refuse"
            );
        } else {
            minisign_verify::PublicKey::decode(key.trim())
                .expect("committed public key must be a valid minisign key");
        }
    }

    #[test]
    fn prerelease_does_not_outrank_its_release() {
        // "1.1.0-rc1" parses as 1.1.0.0 — equal to 1.1.0 on the first three
        // components, so it must not be offered as an upgrade from 1.1.0.
        assert!(!is_newer("1.1.0-rc1", "1.1.0"));
    }
}

// ── UI-facing state ───────────────────────────────────────────────────────

/// What the update UI is doing right now. Owned by the app, polled once per
/// frame, and rendered by both the banner and the Settings card.
#[derive(Default)]
pub struct UpdateState {
    job: Option<Job>,
    /// A release newer than this build, once a check has found one.
    pub available: Option<Update>,
    /// Human-readable outcome of the last action.
    pub message: String,
    /// True while a check or install is running.
    pub busy: bool,
    /// Set once the new binary is in place and a restart is all that is left.
    pub installed: bool,
    /// The user closed the banner for this release.
    pub banner_dismissed: bool,
    /// The user asked to restart into the new binary. Handled centrally by
    /// the app at the end of the frame, which shuts the daemon down before
    /// `restart()` replaces the process image — see that function's contract.
    pub restart_requested: bool,
}

impl UpdateState {
    pub fn start_check(&mut self) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.installed = false;
        self.message = "Checking for updates…".into();
        self.job = Some(check());
    }

    pub fn start_install(&mut self) {
        let Some(update) = self.available.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = format!("Downloading {}…", update.tag);
        self.job = Some(install(update));
    }

    /// Poll the worker. Returns true when something changed, so the caller
    /// can request a repaint.
    pub fn poll(&mut self) -> bool {
        let Some(job) = &self.job else {
            return false;
        };
        let Some(status) = job.poll() else {
            return false;
        };
        self.job = None;
        self.busy = false;
        match status {
            Status::UpToDate => {
                self.available = None;
                self.message = format!("Up to date — v{} is the latest.", current_version());
            }
            Status::Available(u) => {
                self.message = format!("v{} is available.", u.version);
                self.banner_dismissed = false;
                self.available = Some(*u);
            }
            Status::Installed => {
                self.installed = true;
                let tag = self
                    .available
                    .as_ref()
                    .map(|u| u.tag.clone())
                    .unwrap_or_default();
                self.message = format!("{tag} installed — restart to run it.");
            }
            Status::Error(e) => {
                self.message = e;
            }
        }
        true
    }
}
