//! Updates an installed OsmolControl from its GitHub releases. Only the program files are
//! downloaded (gzip-compressed, a few MB each), never the whole package, and every one must
//! match the release's `SHA256SUMS` before anything on disk is replaced.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    env::consts::EXE_SUFFIX,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub const REPOSITORY: &str = "Parapeth962/OsmolControl";
/// Version of the running program.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// Release asset with the SHA-256 of every other asset, in `sha256sum` format.
const SUMS: &str = "SHA256SUMS";
/// Larger downloads are refused; the programs are about 10 MB compressed.
const MAX_DOWNLOAD: u64 = 200 << 20;
/// Larger programs are refused after decompression.
const MAX_PROGRAM: u64 = 500 << 20;
/// Release asset suffix of the programs built for this computer; `None` where none is published.
const PLATFORM: Option<&str> = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("linux-x86_64")
} else if cfg!(target_os = "macos") {
    Some("macos")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some("windows-x86_64")
} else {
    None
};
/// Release asset stem of each program and the file names it has in the packages.
const PROGRAMS: [(&str, &[&str]); 2] = [
    ("osmolcontrol", &["osmolcontrol", "OsmolControl"]),
    ("bikectl", &["bikectl"]),
];
/// MyWhoosh launcher scripts that `install.sh` copies into each MyWhoosh folder.
const LAUNCHERS: [&str; 2] = ["mywhoosh.sh", "ble-bridge.py"];

/// A published release and the download address of each of its files.
#[derive(Debug, Clone)]
pub struct Release {
    /// Without the tag's leading `v`.
    pub version: String,
    assets: HashMap<String, String>,
}

impl Release {
    /// Whether this release is later than the running program.
    pub fn is_newer(&self) -> bool {
        newer(&self.version, CURRENT)
    }

    fn url(&self, asset: &str) -> Result<&str> {
        self.assets
            .get(asset)
            .map(String::as_str)
            .with_context(|| format!("release {} has no {asset}", self.version))
    }
}

/// An installed file and the release asset that replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub asset: String,
    pub path: PathBuf,
}

/// The latest published release.
pub fn latest() -> Result<Release> {
    latest_from(&format!(
        "https://api.github.com/repos/{REPOSITORY}/releases/latest"
    ))
}

/// The release described by a GitHub API address.
pub fn latest_from(url: &str) -> Result<Release> {
    parse_release(&download(url, 1 << 20)?)
}

fn parse_release(json: &[u8]) -> Result<Release> {
    #[derive(Deserialize)]
    struct Api {
        tag_name: String,
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    let api: Api = serde_json::from_slice(json).context("unexpected answer from GitHub")?;
    Ok(Release {
        version: api.tag_name.trim_start_matches('v').to_owned(),
        assets: api
            .assets
            .into_iter()
            .map(|a| (a.name, a.browser_download_url))
            .collect(),
    })
}

/// Whether `candidate` (`0.2.0` or `v0.2.0`) is later than `current`. Anything other than
/// three numbers, such as a pre-release, never is.
pub fn newer(candidate: &str, current: &str) -> bool {
    fn parse(version: &str) -> Option<[u64; 3]> {
        let parts: Vec<u64> = version
            .trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?;
        parts.try_into().ok()
    }
    matches!((parse(candidate), parse(current)), (Some(a), Some(b)) if a > b)
}

/// Files of this installation that an update replaces: the programs in the running one's
/// folder and, on Linux, the MyWhoosh launcher scripts. Call this at start: once the running
/// program has been replaced, Linux reports its path as deleted.
pub fn targets() -> Result<Vec<Target>> {
    let platform = PLATFORM.context("no ready-made OsmolControl is published for this computer")?;
    let exe = std::env::current_exe().context("cannot find the running program")?;
    let dir = exe.parent().context("the running program has no folder")?;
    let mut targets = programs(dir, platform);
    if cfg!(target_os = "linux") {
        targets.extend(launchers(&mywhoosh_roots()));
    }
    Ok(targets)
}

fn programs(dir: &Path, platform: &str) -> Vec<Target> {
    PROGRAMS
        .iter()
        .filter_map(|(stem, names)| {
            // The first match only: Windows and macOS ignore the case of file names.
            let path = names
                .iter()
                .map(|name| dir.join(format!("{name}{EXE_SUFFIX}")))
                .find(|p| p.is_file())?;
            Some(Target {
                asset: format!("{stem}-{platform}{EXE_SUFFIX}.gz"),
                path,
            })
        })
        .collect()
}

/// The MyWhoosh folders `install.sh` uses.
fn mywhoosh_roots() -> Vec<PathBuf> {
    if let Some(root) = std::env::var_os("MYWHOOSH_ROOT") {
        return vec![root.into()];
    }
    let Some(home) = std::env::var_os("HOME") else {
        return vec![];
    };
    ["mywhoosh-linux", "mywhoosh-hd-linux"]
        .iter()
        .map(|dir| Path::new(&home).join("Games").join(dir))
        .collect()
}

fn launchers(roots: &[PathBuf]) -> Vec<Target> {
    roots
        .iter()
        .flat_map(|root| {
            LAUNCHERS.map(|name| Target {
                asset: name.to_owned(),
                path: root.join("bin").join(name),
            })
        })
        .filter(|t| t.path.is_file())
        .collect()
}

/// Downloads `release`'s files for `targets` and checks every one against `SHA256SUMS`; only
/// then are the installed files replaced. A failed download leaves the installation as it was.
pub fn install(release: &Release, targets: &[Target]) -> Result<()> {
    if targets.is_empty() {
        bail!("found nothing to update next to this program");
    }
    let sums = download(release.url(SUMS)?, 1 << 20)?;
    let sums = parse_sums(std::str::from_utf8(&sums).context("unreadable SHA256SUMS")?);
    let mut staged = Vec::new();
    let result = targets.iter().try_for_each(|target| {
        let data = download(release.url(&target.asset)?, MAX_DOWNLOAD)?;
        let expected = sums
            .get(target.asset.as_str())
            .with_context(|| format!("{} is missing from {SUMS}", target.asset))?;
        if hex(&Sha256::digest(&data)) != *expected {
            bail!("{} arrived damaged (checksum mismatch)", target.asset);
        }
        let data = if target.asset.ends_with(".gz") {
            gunzip(&data).with_context(|| format!("unpack {}", target.asset))?
        } else {
            data
        };
        staged.push((stage(&target.path, &data)?, &target.path));
        Ok(())
    });
    if let Err(e) = result {
        for (new, _) in &staged {
            let _ = fs::remove_file(new);
        }
        return Err(e.context("nothing was changed"));
    }
    for (new, path) in &staged {
        replace(new, path)?;
    }
    Ok(())
}

/// Deletes the programs a Windows update had to set aside because they were running.
pub fn remove_leftovers(targets: &[Target]) {
    if cfg!(windows) {
        for target in targets {
            let _ = fs::remove_file(set_aside(&target.path));
        }
    }
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>> {
    let mut response = ureq::get(url)
        .header(
            "User-Agent",
            concat!("OsmolControl/", env!("CARGO_PKG_VERSION")),
        )
        .config()
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .call()
        .with_context(|| format!("cannot download {url}"))?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .with_context(|| format!("cannot download {url}"))
}

/// `sha256sum` output: a hash and a file name per line (`*` marks binary mode).
fn parse_sums(text: &str) -> HashMap<&str, String> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            Some((
                name.trim_start().trim_start_matches('*'),
                hash.to_ascii_lowercase(),
            ))
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn gunzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(MAX_PROGRAM + 1)
        .read_to_end(&mut out)?;
    if out.len() as u64 > MAX_PROGRAM {
        bail!("larger than {} MB", MAX_PROGRAM >> 20);
    }
    Ok(out)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}{suffix}"))
}

fn set_aside(path: &Path) -> PathBuf {
    sibling(path, ".old")
}

/// Writes the new file beside the old one, so the final rename stays on one disk.
fn stage(path: &Path, data: &[u8]) -> Result<PathBuf> {
    let new = sibling(path, ".new");
    fs::write(&new, data).with_context(|| format!("cannot write {}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
    }
    Ok(new)
}

/// A running program keeps its old file open; it uses the new one from its next start.
#[cfg(not(windows))]
fn replace(new: &Path, path: &Path) -> Result<()> {
    fs::rename(new, path).with_context(|| format!("cannot replace {}", path.display()))
}

/// Windows cannot overwrite a running program, but it can rename it out of the way.
#[cfg(windows)]
fn replace(new: &Path, path: &Path) -> Result<()> {
    let old = set_aside(path);
    let _ = fs::remove_file(&old);
    fs::rename(path, &old).with_context(|| format!("cannot replace {}", path.display()))?;
    if let Err(e) = fs::rename(new, path) {
        let _ = fs::rename(&old, path);
        return Err(e).with_context(|| format!("cannot replace {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };

    #[test]
    fn only_later_plain_versions_are_newer() {
        assert!(newer("v0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.9"), "numeric, not text order");
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("v0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(!newer("0.3.0-rc1", "0.2.0"), "pre-release");
        assert!(!newer("0.3", "0.2.0"));
        assert!(!newer("latest", "0.2.0"));
    }

    #[test]
    fn reads_the_github_release() -> Result<()> {
        let release = parse_release(
            br#"{"tag_name":"v0.2.0","name":"x","assets":[
                {"name":"SHA256SUMS","browser_download_url":"https://e/SHA256SUMS","size":1}]}"#,
        )?;
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.url(SUMS)?, "https://e/SHA256SUMS");
        assert!(release.url("bikectl-macos.gz").is_err());
        Ok(())
    }

    #[test]
    fn reads_sha256sum_output() {
        let sums = parse_sums("ABC12  osmolcontrol-macos.gz\nd4e5 *bikectl-macos.gz\n\nbroken\n");
        assert_eq!(sums["osmolcontrol-macos.gz"], "abc12");
        assert_eq!(sums["bikectl-macos.gz"], "d4e5");
        assert_eq!(sums.len(), 2);
    }

    fn scratch(name: &str) -> Result<PathBuf> {
        let dir = std::env::temp_dir().join(format!("bike-update-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    #[test]
    fn finds_the_installed_programs_and_launchers() -> Result<()> {
        let dir = scratch("find")?;
        assert!(programs(&dir, "macos").is_empty());
        fs::write(dir.join(format!("OsmolControl{EXE_SUFFIX}")), "")?;
        let found = programs(&dir, "windows-x86_64");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].asset,
            format!("osmolcontrol-windows-x86_64{EXE_SUFFIX}.gz")
        );

        let (standard, hd) = (dir.join("standard"), dir.join("hd"));
        fs::create_dir_all(standard.join("bin"))?;
        fs::write(standard.join("bin/mywhoosh.sh"), "")?;
        let found = launchers(&[standard.clone(), hd]);
        assert_eq!(
            found,
            [Target {
                asset: "mywhoosh.sh".into(),
                path: standard.join("bin/mywhoosh.sh")
            }]
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// Serves `files` over HTTP and returns a release whose assets point at them.
    fn serve(files: Vec<(&'static str, Vec<u8>)>) -> Result<Release> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base = format!("http://{}", listener.local_addr()?);
        let assets = files
            .iter()
            .map(|(name, _)| (name.to_string(), format!("{base}/{name}")))
            .collect();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = String::new();
                let _ = BufReader::new(&stream).read_line(&mut request);
                let path = request
                    .split(' ')
                    .nth(1)
                    .unwrap_or("")
                    .trim_start_matches('/');
                let body = files.iter().find(|(n, _)| *n == path).map(|(_, b)| b);
                let head = match body {
                    Some(b) => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n", b.len()),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n".into(),
                };
                let _ = stream.write_all(format!("{head}Connection: close\r\n\r\n").as_bytes());
                let _ = stream.write_all(body.map_or(&[][..], |b| b));
            }
        });
        Ok(Release {
            version: "9.9.9".into(),
            assets,
        })
    }

    fn gzip(data: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(data)?;
        Ok(encoder.finish()?)
    }

    #[test]
    fn installs_checked_files_or_nothing() -> Result<()> {
        let dir = scratch("install")?;
        let targets = [
            Target {
                asset: "osmolcontrol-test.gz".into(),
                path: dir.join("osmolcontrol"),
            },
            Target {
                asset: "mywhoosh.sh".into(),
                path: dir.join("mywhoosh.sh"),
            },
        ];
        for t in &targets {
            fs::write(&t.path, "old")?;
        }
        let program = gzip(b"new program")?;
        let script = b"new script".to_vec();
        let sums = |script_hash: &str| {
            format!(
                "{}  osmolcontrol-test.gz\n{script_hash}  mywhoosh.sh\n",
                hex(&Sha256::digest(&program))
            )
            .into_bytes()
        };

        let damaged = serve(vec![
            (SUMS, sums(&"0".repeat(64))),
            ("osmolcontrol-test.gz", program.clone()),
            ("mywhoosh.sh", script.clone()),
        ])?;
        let Err(error) = install(&damaged, &targets) else {
            bail!("a damaged download was installed");
        };
        assert!(
            format!("{error:#}").contains("checksum mismatch"),
            "{error:#}"
        );
        let mut left = fs::read_dir(&dir)?
            .map(|e| Ok(e?.file_name()))
            .collect::<Result<Vec<_>>>()?;
        left.sort();
        assert_eq!(
            left,
            ["mywhoosh.sh", "osmolcontrol"],
            "no staged files left"
        );
        assert_eq!(fs::read(&targets[0].path)?, b"old");

        let good = serve(vec![
            (SUMS, sums(&hex(&Sha256::digest(&script)))),
            ("osmolcontrol-test.gz", program.clone()),
            ("mywhoosh.sh", script.clone()),
        ])?;
        install(&good, &targets)?;
        assert_eq!(fs::read(&targets[0].path)?, b"new program");
        assert_eq!(fs::read(&targets[1].path)?, b"new script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&targets[0].path)?.permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        remove_leftovers(&targets);
        let left = fs::read_dir(&dir)?.count();
        assert_eq!(left, 2, "only the installed files");
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
