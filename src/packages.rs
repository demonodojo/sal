use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

/// Search base for an import: optional dependency name (manifest key / lock name) and root path.
#[derive(Clone, Debug)]
pub struct DepSearchBase {
    pub dep_name: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct ManifestFile {
    #[serde(default)]
    dependencies: HashMap<String, DepSpec>,
}

#[derive(Debug, Deserialize)]
struct DepSpec {
    path: Option<String>,
    git: Option<String>,
    /// GitHub repository `owner/repo`; `version` selects a release (tag `vX.Y.Z`).
    github: Option<String>,
    version: Option<String>,
    /// Subdirectory of the compiler's `std/` (e.g. `"io"`), linked as `sal_modules/<name>`.
    std: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LockFile {
    #[serde(default)]
    pub package: Vec<LockPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockPackage {
    pub name: String,
    pub version: String,
    pub git: String,
    pub rev: String,
    /// SHA-256 of the release tarball (GitHub release dependencies only).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
}

pub fn packages_cache_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SAL_PACKAGES") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs_home().join(".sal").join("packages")
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn read_manifest(project_root: &Path) -> Result<ManifestFile, String> {
    let path = project_root.join("Sal.toml");
    let raw = fs::read_to_string(&path).map_err(|e| format!("Sal.toml: {e}"))?;
    toml::from_str(&raw).map_err(|e| format!("Sal.toml: {e}"))
}

pub fn read_lock(project_root: &Path) -> LockFile {
    let path = project_root.join("Sal.lock");
    let Ok(raw) = fs::read_to_string(&path) else {
        return LockFile::default();
    };
    toml::from_str(&raw).unwrap_or_default()
}

pub fn write_lock(project_root: &Path, lock: &LockFile) -> Result<(), String> {
    let path = project_root.join("Sal.lock");
    let text = toml::to_string_pretty(lock).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())
}

pub fn package_cache_path(name: &str, version: &str) -> PathBuf {
    packages_cache_dir().join(format!("{name}-{version}"))
}

/// Path `[dependencies]` entries plus lock materialized packages (must exist on disk).
pub fn dependency_search_bases(project_root: &Path) -> Vec<DepSearchBase> {
    let mut out = Vec::new();
    if let Ok(manifest) = read_manifest(project_root) {
        for (name, spec) in manifest.dependencies {
            if let Some(rel) = spec.path {
                out.push(DepSearchBase {
                    dep_name: Some(name),
                    path: project_root.join(rel),
                });
            }
        }
    }
    let lock = read_lock(project_root);
    for pkg in lock.package {
        let dir = package_cache_path(&pkg.name, &pkg.version);
        if dir.is_dir() {
            out.push(DepSearchBase {
                dep_name: Some(pkg.name),
                path: dir,
            });
        }
    }
    out
}

fn version_req(raw: &str) -> Result<VersionReq, String> {
    let trimmed = raw.trim();
    let with_caret = if trimmed.starts_with('^') || trimmed.starts_with('=') || trimmed.starts_with('~')
    {
        trimmed.to_string()
    } else {
        format!("^{trimmed}")
    };
    VersionReq::parse(&with_caret).map_err(|e| format!("invalid version requirement `{raw}`: {e}"))
}

fn parse_tag_version(tag: &str) -> Option<Version> {
    let name = tag.strip_prefix("refs/tags/").unwrap_or(tag);
    let ver = name.strip_prefix('v').unwrap_or(name);
    Version::parse(ver).ok()
}

struct RemoteTag {
    version: Version,
    rev: String,
}

fn git_ls_remote_tags(url: &str) -> Result<Vec<RemoteTag>, String> {
    let output = Command::new("git")
        .args(["ls-remote", "--tags", url])
        .output()
        .map_err(|e| format!("git ls-remote failed: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-remote failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut by_ver: HashMap<Version, String> = HashMap::new();
    for line in text.lines() {
        let Some((rev, refname)) = line.split_once('\t') else {
            continue;
        };
        if refname.ends_with("^{}") {
            continue;
        }
        if let Some(v) = parse_tag_version(refname) {
            by_ver.entry(v).or_insert_with(|| rev.to_string());
        }
    }
    let mut tags: Vec<RemoteTag> = by_ver
        .into_iter()
        .map(|(version, rev)| RemoteTag { version, rev })
        .collect();
    tags.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(tags)
}

fn resolve_git_dep(name: &str, url: &str, req: &str) -> Result<LockPackage, String> {
    let requirement = version_req(req)?;
    let tags = git_ls_remote_tags(url)?;
    let mut chosen: Option<&RemoteTag> = None;
    for t in &tags {
        if requirement.matches(&t.version) {
            chosen = Some(t);
        }
    }
    let Some(tag) = chosen else {
        return Err(format!(
            "dependency `{name}`: no tag matching `{req}` in {url}"
        ));
    };
    Ok(LockPackage {
        name: name.to_string(),
        version: tag.version.to_string(),
        git: url.to_string(),
        rev: tag.rev.clone(),
        sha256: String::new(),
    })
}

fn ensure_package_checkout(pkg: &LockPackage) -> Result<(), String> {
    let dir = package_cache_path(&pkg.name, &pkg.version);
    if dir.join(".git").is_dir() {
        let head = Command::new("git")
            .args(["-C", dir.to_str().unwrap(), "rev-parse", "HEAD"])
            .output()
            .map_err(|e| e.to_string())?;
        if head.status.success() {
            let current = String::from_utf8_lossy(&head.stdout).trim().to_string();
            if current == pkg.rev {
                return Ok(());
            }
        }
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(packages_cache_dir()).map_err(|e| e.to_string())?;
    let status = Command::new("git")
        .args([
            "clone",
            "--quiet",
            pkg.git.as_str(),
            dir.to_str().unwrap(),
        ])
        .status()
        .map_err(|e| format!("git clone failed: {e}"))?;
    if !status.success() {
        return Err(format!("git clone failed for `{}`", pkg.git));
    }
    let checkout = Command::new("git")
        .args([
            "-C",
            dir.to_str().unwrap(),
            "checkout",
            "--quiet",
            &pkg.rev,
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if !checkout.success() {
        return Err(format!(
            "git checkout {} failed for package `{}`",
            pkg.rev, pkg.name
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- GitHub releases

const GITHUB_PREFIX: &str = "github:";

/// API root (`SAL_GITHUB_API` overrides it; `file://` roots are used by the tests).
fn github_api() -> String {
    match std::env::var("SAL_GITHUB_API") {
        Ok(v) if !v.is_empty() => v.trim_end_matches('/').to_string(),
        _ => "https://api.github.com".to_string(),
    }
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: sal"]);
    if let Ok(tok) = std::env::var("GITHUB_TOKEN") {
        if !tok.is_empty() {
            cmd.args(["-H", &format!("Authorization: Bearer {tok}")]);
        }
    }
    let out = cmd.arg(url).output().map_err(|e| format!("curl failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "download failed for {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// Highest published (non-draft, non-prerelease) release of `repo` matching `req`.
fn resolve_github_dep(name: &str, repo: &str, req: &str) -> Result<LockPackage, String> {
    let requirement = version_req(req)?;
    let url = format!("{}/repos/{repo}/releases?per_page=100", github_api());
    let body = http_get(&url)?;
    let releases: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("releases of {repo}: invalid JSON: {e}"))?;
    let mut best: Option<(Version, String)> = None;
    for r in releases.as_array().into_iter().flatten() {
        if r["draft"].as_bool().unwrap_or(false) || r["prerelease"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(tag) = r["tag_name"].as_str() else { continue };
        let Some(v) = parse_tag_version(tag) else { continue };
        if requirement.matches(&v) && best.as_ref().map_or(true, |(b, _)| v > *b) {
            best = Some((v, tag.to_string()));
        }
    }
    let Some((version, tag)) = best else {
        return Err(format!(
            "dependency `{name}`: no GitHub release matching `{req}` in {repo}"
        ));
    };
    Ok(LockPackage {
        name: name.to_string(),
        version: version.to_string(),
        git: format!("{GITHUB_PREFIX}{repo}"),
        rev: tag,
        sha256: String::new(),
    })
}

/// Download the release tarball into the cache (if missing) and verify/record its SHA-256.
fn ensure_github_package(pkg: &mut LockPackage) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let dest = package_cache_path(&pkg.name, &pkg.version);
    if dest.is_dir() {
        return Ok(());
    }
    let repo = pkg.git.trim_start_matches(GITHUB_PREFIX);
    let bytes = http_get(&format!("{}/repos/{repo}/tarball/{}", github_api(), pkg.rev))?;
    let sum = hex::encode(Sha256::digest(&bytes));
    if !pkg.sha256.is_empty() && pkg.sha256 != sum {
        return Err(format!(
            "package `{}` {}: checksum mismatch (Sal.lock {}, downloaded {sum})",
            pkg.name, pkg.version, pkg.sha256
        ));
    }
    pkg.sha256 = sum;
    let cache = packages_cache_dir();
    fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    let tmp = cache.join(format!(".{}-{}.tmp", pkg.name, pkg.version));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let tgz = tmp.join("release.tar.gz");
    fs::write(&tgz, &bytes).map_err(|e| e.to_string())?;
    let out = tmp.join("src");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let st = Command::new("tar")
        .args(["xzf"])
        .arg(&tgz)
        .args(["--strip-components=1", "-C"])
        .arg(&out)
        .status()
        .map_err(|e| format!("tar failed: {e}"))?;
    if !st.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("could not extract the release of `{}`", pkg.name));
    }
    fs::rename(&out, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(())
}

/// Resolve git and GitHub-release dependencies, update cache, write `Sal.lock`.
pub fn fetch_packages(project_root: &Path, update: bool) -> Result<(), String> {
    let manifest = read_manifest(project_root)?;
    let old_lock = read_lock(project_root);
    let mut old_by_name: HashMap<String, LockPackage> = old_lock
        .package
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect();

    let mut packages: Vec<LockPackage> = Vec::new();

    for (name, spec) in manifest.dependencies {
        if let (Some(repo), Some(ver)) = (spec.github.clone(), spec.version.clone()) {
            let src = format!("{GITHUB_PREFIX}{repo}");
            let reuse = if update { None } else { old_by_name.remove(&name) };
            let mut pkg = match reuse {
                Some(e) if e.git == src && version_req(&ver)?.matches(&Version::parse(&e.version).unwrap_or(Version::new(0, 0, 0))) => e,
                _ => resolve_github_dep(&name, &repo, &ver)?,
            };
            ensure_github_package(&mut pkg)?;
            packages.push(pkg);
            continue;
        }
        let (Some(url), Some(ver)) = (spec.git, spec.version) else {
            continue;
        };
        let pkg = if update {
            resolve_git_dep(&name, &url, &ver)?
        } else if let Some(existing) = old_by_name.remove(&name) {
            if existing.git == url {
                existing
            } else {
                resolve_git_dep(&name, &url, &ver)?
            }
        } else {
            resolve_git_dep(&name, &url, &ver)?
        };
        ensure_package_checkout(&pkg)?;
        packages.push(pkg);
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));
    write_lock(project_root, &LockFile { package: packages })
}

/// Compiler `std/` directory (`SAL_STD` overrides the build-time location).
pub fn std_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SAL_STD") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("std")
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) -> Result<(), String> {
    match fs::symlink_metadata(link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::remove_file(link).map_err(|e| format!("{}: {e}", link.display()))?;
        }
        Ok(_) => {
            return Err(format!(
                "{} exists and is not a symlink; remove it or rename the dependency",
                link.display()
            ));
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("{} -> {}: {e}", link.display(), target.display()))
}

#[cfg(not(unix))]
fn link_dir(_target: &Path, _link: &Path) -> Result<(), String> {
    Err("sal install needs symlinks (unix only)".into())
}

/// Fetch git dependencies, then link every dependency under `sal_modules/<name>`.
/// Returns the installed `(name, target)` pairs, sorted by name.
pub fn install_packages(
    project_root: &Path,
    update: bool,
) -> Result<Vec<(String, PathBuf)>, String> {
    fetch_packages(project_root, update)?;
    let manifest = read_manifest(project_root)?;
    let lock = read_lock(project_root);
    let modules = project_root.join("sal_modules");
    fs::create_dir_all(&modules).map_err(|e| format!("sal_modules: {e}"))?;
    let mut installed = Vec::new();
    for (name, spec) in manifest.dependencies {
        let target = if let Some(rel) = spec.path {
            let p = project_root.join(rel);
            p.canonicalize()
                .map_err(|e| format!("dependency `{name}`: path {}: {e}", p.display()))?
        } else if spec.git.is_some() || spec.github.is_some() {
            let pkg = lock
                .package
                .iter()
                .find(|p| p.name == name)
                .ok_or_else(|| format!("dependency `{name}` missing from Sal.lock"))?;
            package_cache_path(&pkg.name, &pkg.version)
        } else if let Some(sub) = spec.std {
            let sub = sub.trim_matches('/');
            if sub.is_empty() || sub.contains("..") {
                return Err(format!("dependency `{name}`: invalid std subdirectory"));
            }
            let p = std_dir().join(sub);
            if !p.is_dir() {
                return Err(format!(
                    "dependency `{name}`: {} is not a directory (set SAL_STD)",
                    p.display()
                ));
            }
            p
        } else {
            return Err(format!(
                "dependency `{name}` needs `path`, `git` + `version`, `github` + `version`, or `std`"
            ));
        };
        if !target.is_dir() {
            return Err(format!(
                "dependency `{name}`: {} is not a directory",
                target.display()
            ));
        }
        link_dir(&target, &modules.join(&name))?;
        installed.push((name, target));
    }
    installed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lock_serializes_nonempty() {
        let text = toml::to_string_pretty(&LockFile::default()).expect("serialize");
        assert!(!text.is_empty(), "empty lock should still serialize");
    }

    #[test]
    fn lock_file_roundtrip() {
        let lock = LockFile {
            package: vec![LockPackage {
                name: "dep".into(),
                version: "0.2.0".into(),
                git: "file:///tmp/x".into(),
                rev: "abc".into(),
                sha256: String::new(),
            }],
        };
        let text = toml::to_string_pretty(&lock).expect("serialize");
        let back: LockFile = toml::from_str(&text).expect("parse");
        assert_eq!(back.package.len(), 1);
    }

    #[test]
    fn manifest_parses_git_dependency() {
        let raw = r#"
[package]
name = "consumer"
version = "0.1.0"

[dependencies]
dep = { git = "file:///tmp/x", version = "0.2" }
"#;
        let m: ManifestFile = toml::from_str(raw).expect("parse");
        assert_eq!(m.dependencies.len(), 1);
        let spec = m.dependencies.get("dep").expect("dep");
        assert_eq!(spec.git.as_deref(), Some("file:///tmp/x"));
        assert_eq!(spec.version.as_deref(), Some("0.2"));
    }
}

pub fn add_git_dependency(
    project_root: &Path,
    name: &str,
    git: &str,
    version: &str,
) -> Result<(), String> {
    let manifest_path = project_root.join("Sal.toml");
    let mut raw = if manifest_path.is_file() {
        fs::read_to_string(&manifest_path).map_err(|e| e.to_string())?
    } else {
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n".to_string()
    };
    let line = format!("{name} = {{ git = \"{git}\", version = \"{version}\" }}\n");
    if raw.contains("[dependencies]") {
        if !raw.ends_with('\n') {
            raw.push('\n');
        }
        raw.push_str(&line);
    } else {
        if !raw.ends_with('\n') {
            raw.push('\n');
        }
        raw.push_str("\n[dependencies]\n");
        raw.push_str(&line);
    }
    fs::write(&manifest_path, raw).map_err(|e| e.to_string())?;
    fetch_packages(project_root, false)
}
