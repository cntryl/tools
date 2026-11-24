use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug)]
pub(super) struct CargoScope {
    pub(super) workspace_root: PathBuf,
    pub(super) member_roots: BTreeSet<PathBuf>,
    pub(super) test_files: BTreeSet<PathBuf>,
}

#[derive(Deserialize)]
struct Metadata {
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    manifest_path: PathBuf,
    targets: Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    kind: Vec<String>,
    src_path: PathBuf,
}

pub(super) fn discover(cwd: &Path) -> Result<Option<CargoScope>> {
    if !has_ancestor_manifest(cwd)? {
        return Ok(None);
    }

    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
            "--locked",
        ])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to start Cargo metadata in {}", cwd.display()))?;

    if !output.status.success() {
        return Err(anyhow!(
            "Cargo metadata failed in {} with {}: {}",
            cwd.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let metadata: Metadata =
        serde_json::from_slice(&output.stdout).context("failed to parse Cargo metadata JSON")?;
    let packages_by_id: BTreeMap<&str, &Package> = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package))
        .collect();

    let mut member_roots = BTreeSet::new();
    let mut test_files = BTreeSet::new();
    for member_id in metadata.workspace_members {
        let package = packages_by_id.get(member_id.as_str()).with_context(|| {
            format!("Cargo metadata workspace member {member_id:?} has no resolved package")
        })?;
        let member_root = package
            .manifest_path
            .parent()
            .ok_or_else(|| {
                anyhow!(
                    "Cargo package manifest has no parent: {}",
                    package.manifest_path.display()
                )
            })?
            .to_path_buf();
        member_roots.insert(member_root);

        for target in &package.targets {
            if target.kind.iter().any(|kind| kind == "test") {
                test_files.insert(target.src_path.clone());
            }
        }
    }

    Ok(Some(CargoScope {
        workspace_root: metadata.workspace_root,
        member_roots,
        test_files,
    }))
}

fn has_ancestor_manifest(cwd: &Path) -> Result<bool> {
    for directory in cwd.ancestors() {
        if has_manifest(directory)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn has_manifest(directory: &Path) -> Result<bool> {
    let manifest = directory.join("Cargo.toml");
    match fs::metadata(&manifest) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect Cargo manifest path {}",
                manifest.display()
            )
        }),
    }
}
