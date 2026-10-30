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
    version: Option<String>,
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

/// Installation root containing `std/prelude.sal`.
pub fn std_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("SAL_HOME") {
        let p = PathBuf::from(&home);
        if p.join("std/prelude.sal").is_file() {
            return Some(p);
        }
    }
    let baked = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if baked.join("std/prelude.sal").is_file() {
        return Some(baked);
    }
    None
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

pub fn std_search_base() -> Option<DepSearchBase> {
    std_home().map(|p| DepSearchBase {
        dep_name: Some("std".to_string()),
        path: p.join("std"),
    })
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

/// Resolve git dependencies, update cache, write `Sal.lock`.
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
