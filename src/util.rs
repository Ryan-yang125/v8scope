use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

use crate::contract::Artifact;

pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    use std::io::Write;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn atomic_write(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    file.write_all(content)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub fn sha256_file(path: &Path) -> anyhow::Result<(u64, String)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let bytes = std::io::copy(&mut file, &mut hasher)?;
    Ok((bytes, hex::encode(hasher.finalize())))
}

pub fn validate_node_command(command: &[String]) -> anyhow::Result<()> {
    let executable = command.first().context("target command is empty")?;
    let name = Path::new(executable)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or(executable)
        .to_ascii_lowercase();
    if !matches!(name.as_str(), "node" | "node.exe") {
        bail!("target must be a Node.js executable; received {executable:?}");
    }
    Ok(())
}

pub fn safe_name(value: &str) -> String {
    let result: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = result.trim_matches('-');
    if trimmed.is_empty() {
        "run".into()
    } else {
        trimmed.chars().take(64).collect()
    }
}

pub fn collect_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn visit(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit(root, &path, files)?;
            } else if path.file_name() != Some(OsStr::new("manifest.json")) {
                files.push(path.strip_prefix(root)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

pub fn verify_artifacts(root: &Path, expected: &[Artifact]) -> anyhow::Result<()> {
    let actual = collect_files(root)?;
    let expected_paths = expected
        .iter()
        .map(|artifact| PathBuf::from(&artifact.path))
        .collect::<Vec<_>>();
    if actual != expected_paths {
        bail!(
            "artifact inventory differs from manifest in {}",
            root.display()
        );
    }
    for artifact in expected {
        let (bytes, sha256) = sha256_file(&root.join(&artifact.path))?;
        if bytes != artifact.bytes || sha256 != artifact.sha256 {
            bail!("artifact integrity check failed for {}", artifact.path);
        }
    }
    Ok(())
}

pub fn node_option(value: &Path) -> String {
    let value = value.to_string_lossy();
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
