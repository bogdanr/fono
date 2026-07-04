// SPDX-License-Identifier: GPL-3.0-only
//! macOS implementation of `fono install` / `fono uninstall` —
//! per-user self-installer. No sudo required.
//!
//! What an install does (macOS port plan Phases 9 + 11.4):
//!
//! 1. Assembles a **`~/Applications/Fono.app` bundle** around the
//!    running binary, with a fixed bundle id (`org.fono.app`) and the
//!    mandatory `NSMicrophoneUsageDescription` — a bundled app is
//!    killed by the OS on first mic access without it, and the bundle
//!    is what makes TCC permission grants attribute to *Fono* instead
//!    of Terminal.
//! 2. Ensures a **stable local code-signing identity**
//!    (`fono-local-signing`, a self-signed certificate in a dedicated
//!    keychain) and signs the bundle with it. TCC stores the app's
//!    designated requirement at grant time and re-checks it per launch,
//!    so a stable certificate ⇒ the Accessibility grant survives every
//!    update. Falls back to an ad-hoc signature (grant re-toggle needed
//!    after updates) when certificate setup fails — install never
//!    aborts over signing.
//! 3. Writes a **LaunchAgent** at
//!    `~/Library/LaunchAgents/org.fono.daemon.plist` (`RunAtLoad`,
//!    `KeepAlive` on crash) so the daemon starts at login, and
//!    bootstraps it immediately when a GUI session exists.
//! 4. Best-effort **CLI symlink** at `/usr/local/bin/fono` (needs a
//!    writable `/usr/local/bin`; prints the manual command otherwise).
//!
//! `fono uninstall` is filesystem-driven, mirroring Linux: it boots the
//! agent out, removes the plist / bundle / symlink / reproducible cache
//! (`~/.cache/fono`), and keeps `~/.config/fono` + history. The signing
//! keychain is also kept so a re-install reuses the same identity and
//! previously-granted permissions still match.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::install::InstallModeArg;

// ---------------------------------------------------------------------
// Layout (per-user, $HOME-relative)
// ---------------------------------------------------------------------

/// Bundle id — must stay fixed forever: TCC keys grants on it (together
/// with the signing certificate).
const BUNDLE_ID: &str = "org.fono.app";
/// LaunchAgent label (also the plist file stem).
const AGENT_LABEL: &str = "org.fono.daemon";
/// Common name of the local self-signed code-signing certificate.
const CERT_NAME: &str = "fono-local-signing";
/// Dedicated keychain holding the certificate + key. A separate
/// keychain (instead of the login keychain) lets us unlock it and set
/// the key partition list non-interactively, so signing never pops a
/// keychain password dialog — including during unattended `fono update`.
const SIGNING_KEYCHAIN: &str = "fono-signing.keychain-db";
/// Best-effort CLI symlink; `/usr/local/bin` is user-writable on many
/// setups (Homebrew-on-Intel legacy) but not all.
const CLI_SYMLINK: &str = "/usr/local/bin/fono";

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| anyhow!("$HOME is not set; cannot resolve per-user install paths"))
}

fn app_bundle_dir(home: &Path) -> PathBuf {
    home.join("Applications").join("Fono.app")
}

fn bundle_binary(home: &Path) -> PathBuf {
    app_bundle_dir(home).join("Contents").join("MacOS").join("fono")
}

fn agent_plist_path(home: &Path) -> PathBuf {
    home.join("Library").join("LaunchAgents").join(format!("{AGENT_LABEL}.plist"))
}

fn log_file(home: &Path) -> PathBuf {
    home.join("Library").join("Logs").join("fono.log")
}

// ---------------------------------------------------------------------
// Embedded plists
// ---------------------------------------------------------------------

/// `Contents/Info.plist` for the assembled bundle. `LSUIElement` keeps
/// Fono out of the Dock and Cmd+Tab (menu-bar app); the microphone
/// usage string is mandatory for bundled apps — first mic access
/// crashes without it.
fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>{BUNDLE_ID}</string>
	<key>CFBundleName</key>
	<string>Fono</string>
	<key>CFBundleDisplayName</key>
	<string>Fono</string>
	<key>CFBundleExecutable</key>
	<string>fono</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSUIElement</key>
	<true/>
	<key>NSMicrophoneUsageDescription</key>
	<string>Fono records your voice while you hold or toggle the dictation hotkey, transcribes it, and types the text for you. Audio stays on this Mac unless you configure a cloud provider.</string>
</dict>
</plist>
"#
    )
}

/// The LaunchAgent plist. `RunAtLoad` starts the daemon at login;
/// `KeepAlive.SuccessfulExit=false` restarts it after crashes but
/// honours a deliberate `fono quit` (exit 0). `LimitLoadToSessionType
/// Aqua` scopes it to real GUI logins — SSH sessions never spawn it.
fn launch_agent_plist(home: &Path) -> String {
    let bin = bundle_binary(home);
    let log = log_file(home);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{bin}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#,
        bin = bin.display(),
        log = log.display(),
    )
}

// ---------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------

fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("mkdir -p {}", parent.display()))?;
    }
    let dir = path.parent().ok_or_else(|| anyhow!("path {} has no parent dir", path.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".fono-install-")
        .tempfile_in(dir)
        .with_context(|| format!("create temp file in {}", dir.display()))?;
    tmp.as_file_mut()
        .write_all(bytes)
        .with_context(|| format!("write temp file for {}", path.display()))?;
    tmp.as_file_mut().flush().ok();
    tmp.as_file_mut().sync_all().ok();
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {:o} {}", mode, tmp.path().display()))?;
    tmp.persist(path).map_err(|e| anyhow!("persist into {}: {}", path.display(), e.error))?;
    Ok(())
}

fn try_run(prog: &str, args: &[&str]) -> bool {
    Command::new(prog)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a command, capture stdout+stderr. `Ok((success, combined))` on
/// spawn success.
fn run_out(prog: &str, args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new(prog).args(args).output().with_context(|| format!("spawn {prog}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

fn current_uid() -> u32 {
    // SAFETY: getuid is async-signal-safe and always succeeds.
    unsafe { libc_getuid() }
}

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn refuse_if_package_managed() -> Result<()> {
    let exe = std::env::current_exe().context("resolve current_exe")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if fono_update::is_package_managed(&exe) {
        bail!(
            "{} is managed by Homebrew; update through `brew upgrade fono` \
             instead of running `fono install`",
            exe.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Signing identity (macOS port plan Task 11.4, install side)
// ---------------------------------------------------------------------

/// How the assembled bundle ends up signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signing {
    /// Signed with the stable local certificate — TCC grants survive
    /// updates.
    LocalCert,
    /// Ad-hoc fallback — works, but the Accessibility toggle must be
    /// re-granted after every update.
    AdHoc,
}

/// True iff the `fono-local-signing` identity is already usable for
/// codesigning (in any keychain on the search list).
///
/// Deliberately *not* `find-identity -v`: a self-signed cert shows as
/// `CSSMERR_TP_NOT_TRUSTED` there unless the user has blessed it in a
/// GUI session (trust-settings writes are denied headless), yet
/// `codesign` signs with it regardless — verified on the bench. TCC
/// only records the designated requirement; it never walks the trust
/// chain, so an "untrusted" identity is fully fit for purpose.
fn signing_identity_present() -> bool {
    run_out("security", &["find-identity", "-p", "codesigning"])
        .map(|(ok, out)| ok && out.contains(CERT_NAME))
        .unwrap_or(false)
}

/// Create the self-signed code-signing certificate + dedicated keychain.
///
/// The keychain gets an empty password: the key can only sign *this
/// user's* Fono bundle, and anyone with user-level file access could
/// replace the binary outright anyway — a password would only re-add
/// the interactive prompt this scheme exists to avoid. Everything runs
/// non-interactively so it works over SSH and inside `fono update`.
#[allow(clippy::too_many_lines)] // linear once-only setup script; splitting obscures it
fn create_signing_identity(home: &Path) -> Result<()> {
    let tmp = tempfile::tempdir().context("create temp dir for certificate generation")?;
    let cnf = tmp.path().join("openssl.cnf");
    let key = tmp.path().join("key.pem");
    let cert = tmp.path().join("cert.pem");
    let p12 = tmp.path().join("fono.p12");

    std::fs::write(
        &cnf,
        "[req]\ndistinguished_name = dn\nx509_extensions = v3_codesign\nprompt = no\n\
         [dn]\nCN = fono-local-signing\n\
         [v3_codesign]\nkeyUsage = critical,digitalSignature\n\
         extendedKeyUsage = critical,codeSigning\nbasicConstraints = critical,CA:false\n",
    )
    .context("write openssl config")?;

    // 10-year self-signed cert. macOS ships LibreSSL as `openssl`.
    let (ok, out) = run_out(
        "openssl",
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-days",
            "3650",
            "-nodes",
            "-config",
            cnf.to_str().unwrap_or_default(),
            "-keyout",
            key.to_str().unwrap_or_default(),
            "-out",
            cert.to_str().unwrap_or_default(),
        ],
    )?;
    if !ok {
        bail!("openssl certificate generation failed: {}", out.trim());
    }

    // `security import` only takes PKCS#12 for private keys; LibreSSL's
    // pkcs12 needs a non-empty export password (the value is irrelevant
    // — the p12 is deleted with the temp dir).
    let (ok, out) = run_out(
        "openssl",
        &[
            "pkcs12",
            "-export",
            "-inkey",
            key.to_str().unwrap_or_default(),
            "-in",
            cert.to_str().unwrap_or_default(),
            "-out",
            p12.to_str().unwrap_or_default(),
            "-name",
            CERT_NAME,
            "-passout",
            "pass:fono",
        ],
    )?;
    if !ok {
        bail!("openssl pkcs12 export failed: {}", out.trim());
    }

    // Dedicated keychain: create (idempotent-ish — reuse if present),
    // never auto-lock, unlock now.
    let kc_path = home.join("Library").join("Keychains").join(SIGNING_KEYCHAIN);
    if !kc_path.exists() {
        let (ok, out) = run_out("security", &["create-keychain", "-p", "", SIGNING_KEYCHAIN])?;
        if !ok {
            bail!("security create-keychain failed: {}", out.trim());
        }
    }
    // No auto-lock timeout, so unattended `fono update` re-signs work.
    let _ = try_run("security", &["set-keychain-settings", SIGNING_KEYCHAIN]);
    let (ok, out) = run_out("security", &["unlock-keychain", "-p", "", SIGNING_KEYCHAIN])?;
    if !ok {
        bail!("security unlock-keychain failed: {}", out.trim());
    }

    let (ok, out) = run_out(
        "security",
        &[
            "import",
            p12.to_str().unwrap_or_default(),
            "-k",
            SIGNING_KEYCHAIN,
            "-P",
            "fono",
            "-T",
            "/usr/bin/codesign",
        ],
    )?;
    if !ok {
        bail!("security import failed: {}", out.trim());
    }

    // Allow Apple's signing tools to use the key without a per-use
    // confirmation dialog (the classic CI incantation).
    let (ok, out) = run_out(
        "security",
        &[
            "set-key-partition-list",
            "-S",
            "apple-tool:,apple:,codesign:",
            "-s",
            "-k",
            "",
            SIGNING_KEYCHAIN,
        ],
    )?;
    if !ok {
        bail!("security set-key-partition-list failed: {}", out.trim());
    }

    // Put the keychain on the user search list (keeping what's there)
    // so `codesign` and `security find-identity` can see it.
    add_keychain_to_search_list();

    // Deliberately no `security add-trusted-cert`: writing trust
    // settings requires GUI authorization (denied headless, an extra
    // password dialog otherwise), and codesign + TCC are both happy
    // with the identity untrusted — TCC records the designated
    // requirement without walking the trust chain.

    Ok(())
}

/// Append the signing keychain to the user keychain search list if it
/// isn't already on it.
fn add_keychain_to_search_list() {
    let Ok((true, current)) = run_out("security", &["list-keychains", "-d", "user"]) else {
        return;
    };
    if current.contains(SIGNING_KEYCHAIN) {
        return;
    }
    let mut existing: Vec<String> = current
        .lines()
        .map(|l| l.trim().trim_matches('"').to_string())
        .filter(|l| !l.is_empty())
        .collect();
    existing.push(SIGNING_KEYCHAIN.to_string());
    let mut args: Vec<&str> = vec!["list-keychains", "-d", "user", "-s"];
    args.extend(existing.iter().map(String::as_str));
    let _ = try_run("security", &args);
}

/// Ensure the stable signing identity exists; report which signing mode
/// the bundle will get. Never fails the install.
fn ensure_signing_identity(home: &Path) -> Signing {
    if signing_identity_present() {
        return Signing::LocalCert;
    }
    match create_signing_identity(home) {
        Ok(()) if signing_identity_present() => Signing::LocalCert,
        Ok(()) => {
            eprintln!(
                "  · created certificate but `security find-identity` can't see it; \
                 falling back to ad-hoc signing"
            );
            Signing::AdHoc
        }
        Err(e) => {
            eprintln!("  · local signing certificate setup failed ({e:#}); using ad-hoc signing");
            eprintln!(
                "    (works fine, but macOS will ask you to re-toggle the Accessibility \
                 permission after each update)"
            );
            Signing::AdHoc
        }
    }
}

/// Sign the bundle. With [`Signing::LocalCert`] a failed cert signing
/// degrades to ad-hoc rather than aborting.
fn sign_bundle(bundle: &Path, signing: Signing) -> Signing {
    if signing == Signing::LocalCert {
        let _ = try_run("security", &["unlock-keychain", "-p", "", SIGNING_KEYCHAIN]);
        let (ok, out) = run_out(
            "codesign",
            &["--force", "--sign", CERT_NAME, "--identifier", BUNDLE_ID, &bundle.to_string_lossy()],
        )
        .unwrap_or_else(|_| (false, "spawn failed".into()));
        if ok {
            return Signing::LocalCert;
        }
        eprintln!("  · codesign with {CERT_NAME} failed ({}); using ad-hoc", out.trim());
    }
    let ok = try_run(
        "codesign",
        &["--force", "--sign", "-", "--identifier", BUNDLE_ID, &bundle.to_string_lossy()],
    );
    if !ok {
        eprintln!("  · ad-hoc codesign failed; the bundle keeps the binary's linker signature");
    }
    Signing::AdHoc
}

// ---------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------

/// The `.app` bundle root that `path` lives inside, if any.
fn enclosing_bundle(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
        .map(Path::to_path_buf)
}

/// Re-sign the app bundle after `fono update` swapped the binary inside
/// it (macOS port plan Task 10.2 / the update half of Task 11.4).
///
/// Replacing `Contents/MacOS/fono` invalidates the bundle's code
/// signature; left broken, the next launch would fail TCC's
/// designated-requirement re-check and silently void the Accessibility
/// grant. Re-signing with the *same* local certificate (created by
/// `fono install`) restores an identical designated requirement, so the
/// grant survives. `new_version` (the tag just installed) refreshes the
/// bundle's version strings before sealing — the requirement only pins
/// bundle id + certificate, so this is grant-neutral.
///
/// Returns `None` when `installed_at` is a bare binary outside any
/// `.app` bundle (nothing to re-sign — grants were never
/// bundle-attributed), `Some(true)` when the stable local identity
/// sealed the bundle, `Some(false)` on the ad-hoc fallback (macOS will
/// ask the user to re-toggle Accessibility once).
pub fn resign_after_update(installed_at: &Path, new_version: Option<&str>) -> Option<bool> {
    let bundle = enclosing_bundle(installed_at)?;
    if let Some(v) = new_version {
        let plist = bundle.join("Contents").join("Info.plist");
        if plist.exists() {
            let _ = write_atomic(&plist, info_plist(v).as_bytes(), 0o644);
        }
    }
    let want = if signing_identity_present() { Signing::LocalCert } else { Signing::AdHoc };
    Some(sign_bundle(&bundle, want) == Signing::LocalCert)
}

pub fn run_install(mode: InstallModeArg, dry_run: bool) -> Result<()> {
    if mode == InstallModeArg::Server {
        bail!(
            "`fono install --server` (headless Wyoming server with a system service) is \
             Linux-only. On macOS run `fono install` for the per-user app, or run \
             `fono` manually with `[server.wyoming].enabled = true` in your config."
        );
    }

    let home = home_dir()?;
    let bundle = app_bundle_dir(&home);
    let plist = agent_plist_path(&home);

    if dry_run {
        println!("fono install --dry-run (macOS, per-user) — would perform:");
        println!("  · assemble app bundle -> {}", bundle.display());
        println!("  · ensure local code-signing certificate `{CERT_NAME}` (created once)");
        println!("  · codesign the bundle (stable identity: permission grants survive updates)");
        println!("  · write LaunchAgent -> {} (starts at login)", plist.display());
        println!("  · launchctl bootstrap gui/{} (start now; skipped over SSH)", current_uid());
        println!("  · symlink {CLI_SYMLINK} -> bundle binary (best-effort)");
        return Ok(());
    }

    refuse_if_package_managed()?;

    eprintln!("→ installing fono (macOS, per-user — no sudo needed)");

    // Ask any running daemon to exit before we swap its binary.
    shutdown_existing_daemon(&home);

    // 1. Bundle
    let bin = bundle_binary(&home);
    let src = std::env::current_exe().context("resolve current_exe")?;
    let src = std::fs::canonicalize(&src).unwrap_or(src);
    if src != bin {
        let bytes = std::fs::read(&src)
            .with_context(|| format!("read running binary at {}", src.display()))?;
        write_atomic(&bin, &bytes, 0o755)?;
    }
    write_atomic(
        &bundle.join("Contents").join("Info.plist"),
        info_plist(env!("CARGO_PKG_VERSION")).as_bytes(),
        0o644,
    )?;
    eprintln!("  · {}", bundle.display());

    // 2. Stable signing identity + signature
    let signing = ensure_signing_identity(&home);
    match sign_bundle(&bundle, signing) {
        Signing::LocalCert => {
            eprintln!("  · signed with `{CERT_NAME}` (permission grants survive updates)");
        }
        Signing::AdHoc => eprintln!("  · signed ad-hoc"),
    }

    // 3. LaunchAgent
    write_atomic(&plist, launch_agent_plist(&home).as_bytes(), 0o644)?;
    eprintln!("  · {}", plist.display());
    let started = bootstrap_agent(&plist);

    // 4. CLI symlink (best-effort)
    link_cli(&bin);

    println!();
    if started {
        println!("Fono installed and started. It will also start automatically at login.");
    } else {
        println!("Fono installed. It will start automatically at your next login");
        println!("(no GUI session right now, so it was not started immediately).");
    }
    println!();
    println!("Two one-time permissions make dictation fully hands-free:");
    println!("  1. Microphone — macOS asks with a native Allow prompt on your first");
    println!("     dictation. One click.");
    println!("  2. Accessibility — lets Fono type the transcribed text for you.");
    println!("     Flip the Fono toggle once under:");
    println!("       System Settings → Privacy & Security → Accessibility");
    println!("     (or run: open \\");
    println!(
        "      \"x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility\")"
    );
    println!("Until then, dictation still works — the text lands on the clipboard");
    println!("and a notification reminds you to paste with Cmd+V.");
    println!();
    println!("Per-user config lives under ~/.config/fono/, history under");
    println!("~/.local/share/fono/. Check status anytime with `fono doctor`.");
    Ok(())
}

pub fn run_uninstall(dry_run: bool) -> Result<()> {
    let home = home_dir()?;
    let bundle = app_bundle_dir(&home);
    let plist = agent_plist_path(&home);
    let cache = home.join(".cache").join("fono");

    let mut targets: Vec<(String, bool)> = Vec::new(); // (description, is_dir)
    if plist.symlink_metadata().is_ok() {
        targets.push((plist.display().to_string(), false));
    }
    if bundle.symlink_metadata().is_ok() {
        targets.push((bundle.display().to_string(), true));
    }
    let symlink_ours =
        std::fs::read_link(CLI_SYMLINK).map(|t| t.starts_with(&bundle)).unwrap_or(false);
    if symlink_ours {
        targets.push((CLI_SYMLINK.to_string(), false));
    }

    if targets.is_empty() {
        bail!(
            "no fono installation detected ({} / {}); nothing to uninstall",
            bundle.display(),
            plist.display()
        );
    }

    if dry_run {
        println!("fono uninstall --dry-run (macOS) — would perform:");
        println!("  · launchctl bootout gui/{}/{AGENT_LABEL} (stop the agent)", current_uid());
        for (t, _) in &targets {
            println!("  · remove {t}");
        }
        if cache.exists() {
            println!("  · remove {} (reproducible model / hwcheck cache)", cache.display());
        }
        println!("  · keep ~/.config/fono, ~/.local/share/fono, and the signing keychain");
        return Ok(());
    }

    eprintln!("→ uninstalling fono (macOS)");

    // Stop + unregister the agent (both domains, best-effort — over
    // SSH the gui domain doesn't exist).
    let uid = current_uid();
    let _ = try_run("launchctl", &["bootout", &format!("gui/{uid}/{AGENT_LABEL}")]);
    let _ = try_run("launchctl", &["bootout", &format!("user/{uid}/{AGENT_LABEL}")]);
    shutdown_existing_daemon(&home);

    for (t, is_dir) in &targets {
        let res = if *is_dir { std::fs::remove_dir_all(t) } else { std::fs::remove_file(t) };
        match res {
            Ok(()) => eprintln!("  · removed {t}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("  · already gone: {t}");
            }
            Err(e) => eprintln!("  · could not remove {t} ({e}); delete manually"),
        }
    }

    if cache.exists() {
        match std::fs::remove_dir_all(&cache) {
            Ok(()) => eprintln!("  · removed {}", cache.display()),
            Err(e) => eprintln!(
                "  · could not remove {} ({e}); delete manually if no longer needed",
                cache.display()
            ),
        }
    }

    println!();
    println!("Fono uninstalled.");
    println!("Per-user config (~/.config/fono) and history (~/.local/share/fono) are");
    println!("kept. The local signing keychain is kept too, so a future re-install");
    println!("keeps the same identity and your permission grants still match.");
    Ok(())
}

/// One-line install-state summary for `fono doctor`.
#[must_use]
pub fn doctor_state() -> String {
    let exe = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
    let exe_str =
        exe.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<unknown>".into());
    if let Some(ref p) = exe {
        if fono_update::is_package_managed(p) {
            return format!("package-managed ({exe_str})");
        }
    }
    let Ok(home) = home_dir() else {
        return format!("ad-hoc on PATH ({exe_str})");
    };
    let bundle = app_bundle_dir(&home).exists();
    let agent = agent_plist_path(&home).exists();
    match (bundle, agent) {
        (true, true) => {
            format!("self-installed via `fono install` (app bundle + login agent, {exe_str})")
        }
        (true, false) => format!("app bundle present, login agent missing ({exe_str})"),
        (false, true) => format!("login agent present, app bundle missing ({exe_str})"),
        (false, false) => format!("ad-hoc on PATH ({exe_str})"),
    }
}

// ---------------------------------------------------------------------
// Agent + daemon lifecycle helpers
// ---------------------------------------------------------------------

/// Register + start the agent in the caller's GUI domain. Returns
/// true when the daemon is actually running afterwards. Over SSH there
/// is no gui domain — the agent then simply starts at next login.
fn bootstrap_agent(plist: &Path) -> bool {
    let uid = current_uid();
    let gui = format!("gui/{uid}");
    // Re-installs: boot the old registration out first (ignore errors).
    let _ = try_run("launchctl", &["bootout", &format!("{gui}/{AGENT_LABEL}")]);
    let Ok((ok, out)) = run_out("launchctl", &["bootstrap", &gui, &plist.to_string_lossy()]) else {
        return false;
    };
    if !ok {
        // "Bootstrap failed: 125: domain does not exist" et al. — the
        // headless / SSH case. Not an error worth alarming the user.
        tracing::debug!("launchctl bootstrap {gui} failed: {}", out.trim());
        return false;
    }
    eprintln!("  · launchctl bootstrap {gui} ({AGENT_LABEL})");
    true
}

/// Best-effort IPC shutdown of an already-running daemon so binary
/// swaps and agent restarts don't race an old process (mirrors the
/// Linux installer's behaviour).
fn shutdown_existing_daemon(home: &Path) {
    let state_root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"));
    let socket = state_root.join("fono").join("fono.sock");
    if !socket.exists() {
        return;
    }
    let sent = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            return false;
        };
        rt.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                fono_ipc::request_any(std::slice::from_ref(&socket), &fono_ipc::Request::Shutdown),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .is_some()
        })
    })
    .join()
    .unwrap_or(false);
    if sent {
        eprintln!("  · asked existing fono daemon to exit");
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

/// Best-effort `/usr/local/bin/fono` symlink so the CLI works from a
/// terminal. Prints the manual command when the dir isn't writable.
fn link_cli(bin: &Path) {
    let link = Path::new(CLI_SYMLINK);
    match std::fs::read_link(link) {
        Ok(target) if target == bin => {
            eprintln!("  · {CLI_SYMLINK} already points at the bundle");
            return;
        }
        Ok(_) => {
            // Points elsewhere (old install layout?) — replace only if
            // it pointed into our bundle tree; otherwise leave it alone.
            if !std::fs::read_link(link)
                .map(|t| t.starts_with(bin.parent().unwrap_or(bin)))
                .unwrap_or(false)
            {
                eprintln!(
                    "  · {CLI_SYMLINK} exists and isn't ours — left alone; \
                     put {} on your PATH instead",
                    bin.display()
                );
                return;
            }
        }
        Err(_) if link.exists() => {
            eprintln!("  · {CLI_SYMLINK} exists (not a symlink) — left alone");
            return;
        }
        Err(_) => {}
    }
    let _ = std::fs::remove_file(link);
    if std::os::unix::fs::symlink(bin, link).is_ok() {
        eprintln!("  · {CLI_SYMLINK} -> {}", bin.display());
    } else {
        eprintln!("  · could not write {CLI_SYMLINK} (needs sudo); to use the CLI run:");
        eprintln!("      sudo ln -sf {} {CLI_SYMLINK}", bin.display());
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_plist_carries_the_load_bearing_keys() {
        let p = info_plist(env!("CARGO_PKG_VERSION"));
        // Fixed bundle id — TCC grants key on it; never change.
        assert!(p.contains("<string>org.fono.app</string>"));
        // Menu-bar app: no Dock icon.
        assert!(p.contains("LSUIElement"));
        // Mandatory for bundled apps: first mic access crashes without it.
        assert!(p.contains("NSMicrophoneUsageDescription"));
        assert!(p.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn launch_agent_plist_points_at_the_bundle_binary() {
        let home = Path::new("/Users/testuser");
        let p = launch_agent_plist(home);
        assert!(p.contains("<string>org.fono.daemon</string>"));
        assert!(p.contains("/Users/testuser/Applications/Fono.app/Contents/MacOS/fono"));
        assert!(p.contains("RunAtLoad"));
        // Crash-restart but honour a deliberate quit (exit 0).
        assert!(p.contains("SuccessfulExit"));
        // GUI logins only — SSH sessions must not spawn the agent.
        assert!(p.contains("<string>Aqua</string>"));
        assert!(p.contains("/Users/testuser/Library/Logs/fono.log"));
    }

    #[test]
    fn plists_lint_clean_when_plutil_is_available() {
        // Validate both documents with the system linter (always
        // present on macOS; guard kept for safety since this module
        // only compiles on darwin).
        if !try_run("plutil", &["-help"]) {
            return;
        }
        for doc in [info_plist("9.9.9"), launch_agent_plist(Path::new("/Users/t"))] {
            let tmp = tempfile::NamedTempFile::new().expect("tmp");
            std::fs::write(tmp.path(), doc).expect("write");
            assert!(try_run("plutil", &["-lint", &tmp.path().to_string_lossy()]));
        }
    }

    #[test]
    fn server_mode_is_refused() {
        let err = run_install(InstallModeArg::Server, true).unwrap_err();
        assert!(err.to_string().contains("Linux-only"));
    }

    /// Path logic for the update-time re-sign hook: binaries inside a
    /// bundle resolve to the bundle root; bare binaries resolve to None.
    #[test]
    fn enclosing_bundle_detects_bundle_layout() {
        let inside = Path::new("/Users/u/Applications/Fono.app/Contents/MacOS/fono");
        assert_eq!(
            enclosing_bundle(inside).as_deref(),
            Some(Path::new("/Users/u/Applications/Fono.app"))
        );
        assert_eq!(enclosing_bundle(Path::new("/usr/local/bin/fono")), None);
        assert_eq!(enclosing_bundle(Path::new("/Users/u/.cargo/bin/fono")), None);
    }
}
