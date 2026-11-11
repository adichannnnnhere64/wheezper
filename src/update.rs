//! `wheezper --update`: replace this binary with the release CI validated.
//!
//! No installer and no admin rights: the updater reads the public GitHub
//! release, downloads the **raw binary** asset (published next to the tar.gz /
//! zip) plus `SHA256SUMS` from the same release, verifies the hash, writes the
//! new binary next to the running one and renames it into place.
//!
//! Linux/macOS can replace a running executable atomically. Windows cannot: a
//! running image is locked, so the new binary is staged as `wheezper.exe.new`
//! and a detached `cmd` moves it into place once this process exits.
//!
//! Only a *release* build should self-update; a `cargo run` binary would also
//! "update" itself, which is fine (it just overwrites target/).

use anyhow::{anyhow, bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Releases live here; the updater only ever reads public assets from it.
pub const REPO: &str = "adichannnnnhere64/wheezper";
const API_LATEST: &str = "https://api.github.com/repos";
const SUMS_ASSET: &str = "SHA256SUMS";
const USER_AGENT: &str = concat!("wheezper/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(120);

/// Raw binary asset name for this platform. These are published explicitly
/// (the tar.gz/zip carry the same bytes) so the updater needs no archiver.
pub fn asset_name() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("wheezper-linux-amd64"),
        ("windows", "x86_64") => Ok("wheezper-windows-x64.exe"),
        (os, arch) => bail!(
            "no self-update build for {os}/{arch} (only linux-x86_64 and windows-x86_64 are \
             published); download a new binary by hand from https://github.com/{REPO}/releases"
        ),
    }
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    /// Without the leading `v`.
    pub tag: String,
    pub assets: Vec<Asset>,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate { latest: String },
    Updated { version: String, path: PathBuf },
    /// Windows: staged, and a helper replaces the locked exe after exit.
    Staged { version: String, path: PathBuf },
}

/// Entry point for `--update`.
pub fn run() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let asset = asset_name()?;
    let target = std::env::current_exe().context("locating the running binary")?;
    println!("wheezper: running {} ({current})", target.display());
    println!("wheezper: checking https://github.com/{REPO}/releases/latest");

    match apply(
        &format!("{API_LATEST}/{REPO}/releases/latest"),
        current,
        asset,
        &target,
    )? {
        Outcome::UpToDate { latest } => {
            println!("wheezper: already up to date ({current}); latest release is {latest}");
        }
        Outcome::Updated { version, path } => {
            println!("wheezper: updated to {version} — {}", path.display());
        }
        Outcome::Staged { version, path } => {
            println!(
                "wheezper: {version} staged next to {}; it replaces this exe when the process exits",
                path.display()
            );
        }
    }
    Ok(())
}

/// The whole update, with the two endpoints injectable so it is testable
/// without network access.
pub fn apply(api_url: &str, current: &str, asset: &str, target: &Path) -> Result<Outcome> {
    let release = fetch_release(api_url)?;
    if !is_newer(&release.tag, current) {
        return Ok(Outcome::UpToDate {
            latest: release.tag,
        });
    }
    let binary = release.asset(asset).ok_or_else(|| {
        anyhow!(
            "release {} has no '{asset}' asset (has: {}); update by hand: \
             https://github.com/{REPO}/releases",
            release.tag,
            release
                .assets
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let sums_url = release
        .asset(SUMS_ASSET)
        .map(|a| a.url.clone())
        .ok_or_else(|| {
            anyhow!("release {} has no {SUMS_ASSET}: refusing to update unverified bytes", release.tag)
        })?;

    println!(
        "wheezper: downloading {} → {}",
        asset, release.tag
    );
    let checksums = String::from_utf8(fetch(&sums_url)?).context("SHA256SUMS is not UTF-8")?;
    let expected = expected_sha256(&checksums, asset)?;
    let bytes = fetch(&binary.url)?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        bail!(
            "checksum mismatch for {asset}: expected {expected}, got {actual} — refusing to install"
        );
    }
    if bytes.is_empty() {
        bail!("downloaded {asset} is empty");
    }

    let path = install(&bytes, target)?;
    // Windows stages the file and replaces it after this process exits.
    Ok(if cfg!(windows) {
        Outcome::Staged {
            version: release.tag,
            path,
        }
    } else {
        Outcome::Updated {
            version: release.tag,
            path,
        }
    })
}

fn fetch_release(url: &str) -> Result<Release> {
    let body = fetch(url)?;
    parse_release(&String::from_utf8_lossy(&body))
}

pub fn parse_release(json: &str) -> Result<Release> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("release metadata is not valid JSON")?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("release metadata has no tag_name"))?;
    let assets = value
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|asset| {
                    Some(Asset {
                        name: asset.get("name")?.as_str()?.to_string(),
                        url: asset.get("browser_download_url")?.as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        tag: tag.trim_start_matches('v').to_string(),
        assets,
    })
}

fn fetch(url: &str) -> Result<Vec<u8>> {
    let request = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(TIMEOUT))
        .build();
    let resp = request.call().with_context(|| format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let mut buf = Vec::new();
    resp.into_parts()
        .1
        .into_reader()
        .read_to_end(&mut buf)
        .with_context(|| format!("reading {url}"))?;
    Ok(buf)
}

/// `SHA256SUMS` lines look like `<hex>  <name>` (sha256sum output).
pub fn expected_sha256(sums: &str, name: &str) -> Result<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(file)) = (parts.next(), parts.next()) else {
            continue;
        };
        let file = file.trim_start_matches('*');
        if file == name {
            if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("malformed sha256 for {name} in {SUMS_ASSET}");
            }
            return Ok(hash.to_ascii_lowercase());
        }
    }
    bail!("{name} is missing from {SUMS_ASSET}: refusing to update unverified bytes")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Latest is newer than current? Compares the first three dotted numbers, the
/// rest (`-rc1`, build metadata) is ignored: a release tag is `v0.6.2`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let core = v.split(['-', '+']).next().unwrap_or(v);
        let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        )
    };
    parse(latest) > parse(current)
}

/// Write the verified bytes and put them in place of `target`.
#[cfg(unix)]
fn install(bytes: &[u8], target: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = target
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", target.display()))?;
    let staged = dir.join(format!(
        ".{}.new",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "wheezper".into())
    ));
    write_staged(bytes, &staged)?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("marking {} executable", staged.display()))?;
    // Same directory, so this is an atomic rename, not a cross-device copy.
    std::fs::rename(&staged, target).with_context(|| {
        format!(
            "replacing {} (a read-only location — move the binary somewhere writable, \
             or reinstall by hand)",
            target.display()
        )
    })?;
    Ok(target.to_path_buf())
}

#[cfg(windows)]
fn install(bytes: &[u8], target: &Path) -> Result<PathBuf> {
    // A running .exe is locked on Windows, so stage the new one and let a
    // detached shell replace it after this process exits. `ping -n 3` is a
    // 2 s sleep (timeout.exe needs a console that may not exist).
    let staged = target.with_extension("exe.new");
    write_staged(bytes, &staged)?;
    let script = format!(
        "ping -n 3 127.0.0.1 >NUL & move /Y \"{}\" \"{}\"",
        staged.display(),
        target.display()
    );
    std::process::Command::new("cmd")
        .args(["/C", &script])
        .spawn()
        .context("starting the post-exit file replace")?;
    Ok(target.to_path_buf())
}

/// Neither unix nor Windows: `asset_name` already refuses, but the module must
/// still compile (macOS, Termux/aarch64, ...).
#[cfg(not(any(unix, windows)))]
fn install(_bytes: &[u8], _target: &Path) -> Result<PathBuf> {
    bail!("self-update is not supported on this platform; download a new binary by hand")
}

fn write_staged(bytes: &[u8], staged: &Path) -> Result<()> {
    let mut file = std::fs::File::create(staged)
        .with_context(|| format!("creating {}", staged.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", staged.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", staged.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Serve fixed path → bytes routes on 127.0.0.1. The closure receives the
    /// base URL, so a test can put it into the release JSON it serves.
    fn serve(build: impl FnOnce(&str) -> Vec<(&'static str, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let routes: Vec<(String, Vec<u8>)> = build(&base)
            .into_iter()
            .map(|(path, body)| (path.to_string(), body))
            .collect();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut request = String::new();
                let _ = reader.read_line(&mut request);
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                let body = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, body)| body.clone())
                    .unwrap_or_default();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        base
    }

    fn release_json(tag: &str, base: &str) -> Vec<u8> {
        format!(
            r#"{{"tag_name":"{tag}","assets":[
                {{"name":"wheezper-linux-amd64","browser_download_url":"{base}/bin"}},
                {{"name":"SHA256SUMS","browser_download_url":"{base}/SHA256SUMS"}}]}}"#
        )
        .into_bytes()
    }

    fn temp_target(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("wheezper-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let target = dir.join("wheezper");
        std::fs::write(&target, b"old").expect("old binary");
        (dir, target)
    }

    #[test]
    fn compares_dotted_versions_and_tags() {
        assert!(is_newer("0.6.2", "0.6.1"));
        assert!(is_newer("v0.7.0", "0.6.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("0.6.1", "0.6.1"));
        assert!(!is_newer("v0.6.1", "0.6.1"));
        assert!(!is_newer("0.5.9", "0.6.1"));
        assert!(is_newer("0.6.2-rc1", "0.6.1"));
    }

    #[test]
    fn every_platform_has_a_published_asset_name_or_a_clear_error() {
        match asset_name() {
            Ok(name) => assert!(
                matches!(name, "wheezper-linux-amd64" | "wheezper-windows-x64.exe"),
                "unexpected asset name {name}"
            ),
            Err(err) => assert!(format!("{err}").contains("by hand")),
        }
    }

    #[test]
    fn reads_the_hash_for_this_asset_only() {
        let linux = "a".repeat(64);
        let windows = "b".repeat(64);
        let sums = format!(
            "{linux}  wheezper-linux-amd64\n{windows} *wheezper-windows-x64.exe\n"
        );
        assert_eq!(
            expected_sha256(&sums, "wheezper-linux-amd64").unwrap(),
            linux
        );
        assert_eq!(
            expected_sha256(&sums, "wheezper-windows-x64.exe").unwrap(),
            windows
        );
        let sums = sums.as_str();
        assert!(expected_sha256(sums, "other").is_err());
        assert!(expected_sha256("aa wheezper-linux-amd64", "wheezper-linux-amd64").is_err());
    }

    #[test]
    fn parses_release_metadata() {
        let release = parse_release(
            r#"{"tag_name":"v0.9.0","assets":[{"name":"x","browser_download_url":"http://h/x"}]}"#,
        )
        .unwrap();
        assert_eq!(release.tag, "0.9.0");
        assert_eq!(release.asset("x").unwrap().url, "http://h/x");
        assert!(parse_release("{}").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn updates_a_binary_in_place_with_a_verified_download() {
        let new_binary = b"#!/bin/sh\necho updated\n".to_vec();
        let hash = sha256_hex(&new_binary);
        let base = serve(|base| {
            vec![
                ("/latest", release_json("v9.9.9", base)),
                ("/bin", new_binary.clone()),
                ("/SHA256SUMS", format!("{hash}  wheezper-linux-amd64\n").into_bytes()),
            ]
        });
        let (dir, target) = temp_target("ok");

        let outcome = apply(
            &format!("{base}/latest"),
            "0.6.1",
            "wheezper-linux-amd64",
            &target,
        )
        .expect("update");
        assert!(matches!(outcome, Outcome::Updated { ref version, .. } if version == "9.9.9"));
        assert_eq!(std::fs::read(&target).unwrap(), new_binary);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".new"))
            .collect();
        assert!(leftovers.is_empty(), "staging file left behind");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_an_unverified_binary() {
        let binary = b"new".to_vec();
        let base = serve(|base| {
            vec![
                ("/latest", release_json("v9.9.9", base)),
                ("/bin", binary.clone()),
                (
                    "/SHA256SUMS",
                    format!("{}  wheezper-linux-amd64\n", "0".repeat(64)).into_bytes(),
                ),
            ]
        });
        let (dir, target) = temp_target("bad");
        let err = apply(
            &format!("{base}/latest"),
            "0.6.1",
            "wheezper-linux-amd64",
            &target,
        )
        .expect_err("checksum must be verified");
        assert!(format!("{err}").contains("checksum mismatch"), "{err}");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old",
            "the old binary must survive a bad download"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_up_to_date_binary_downloads_nothing() {
        let base = serve(|base| vec![("/latest", release_json("v0.6.1", base))]);
        let (dir, target) = temp_target("same");
        let outcome = apply(
            &format!("{base}/latest"),
            "0.6.1",
            "wheezper-linux-amd64",
            &target,
        )
        .unwrap();
        assert_eq!(
            outcome,
            Outcome::UpToDate {
                latest: "0.6.1".into()
            }
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        std::fs::remove_dir_all(&dir).ok();
    }
}
