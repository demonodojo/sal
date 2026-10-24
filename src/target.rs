//! Target and toolchain configuration, shared by the CLI and library clients.
use sha2::{Digest, Sha256};
use std::{io::Read, path::PathBuf, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Target {
    #[default]
    Linux,
    Soso,
}

impl Target {
    pub fn host() -> Self {
        if cfg!(target_os = "soso") {
            Self::Soso
        } else {
            Self::Linux
        }
    }
    pub fn triple(self) -> &'static str {
        match self {
            Self::Linux => "x86_64-unknown-linux-gnu",
            Self::Soso => "x86_64-unknown-none-elf",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub target: Target,
    pub cc: PathBuf,
    pub linker: PathBuf,
    pub sdk: PathBuf,
}

impl Default for Toolchain {
    fn default() -> Self {
        Self::new(Target::host())
    }
}

impl Toolchain {
    pub fn new(target: Target) -> Self {
        let native = cfg!(target_os = "soso");
        Self {
            target,
            cc: std::env::var_os("SAL_CC")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    if native {
                        "/lib/sal/bin/sal-llc".into()
                    } else {
                        "clang".into()
                    }
                }),
            linker: std::env::var_os("SAL_LD")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    if native {
                        "/bin/wild-soso".into()
                    } else {
                        "rust-lld".into()
                    }
                }),
            sdk: std::env::var_os("SAL_SDK")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/lib/sal".into()),
        }
    }

    /// Computed once per build. SDK manifest includes hashes of all runtime artifacts.
    pub fn identity(&self) -> String {
        let mut h = Sha256::new();
        h.update(format!("sal-target-v1:{self:?}"));
        for tool in [&self.cc, &self.linker] {
            // Native tools may be rebuilt without changing their version string.
            let path = if tool.is_file() {
                Some(tool.clone())
            } else {
                std::env::var_os("PATH").and_then(|p| {
                    std::env::split_paths(&p)
                        .map(|d| d.join(tool))
                        .find(|p| p.is_file())
                })
            };
            if let Some(mut file) = path.and_then(|p| std::fs::File::open(p).ok()) {
                let mut buffer = [0u8; 32768];
                loop {
                    match file.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => h.update(&buffer[..n]),
                    }
                }
            }
            if let Ok(out) = Command::new(tool).arg("--version").output() {
                h.update(&out.stdout);
                h.update(&out.stderr);
            }
        }
        if self.target == Target::Soso {
            h.update(std::fs::read(self.sdk.join("manifest.txt")).unwrap_or_default());
        }
        hex::encode(h.finalize())
    }
}
