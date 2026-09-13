use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};

pub const REPOSITORY: &str = "staFF6773/T7Patch-Rs";
pub const ARCHIVE: &str = "t7patch-windows-x64.zip";
pub const MANIFEST: &str = "update.json";
pub const FILES: [&str; 2] = ["t7patch.exe", "t7patch.dll"];
pub const TARGET: &str = "x86_64-pc-windows-msvc";
pub const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
pub const MAX_FILE: u64 = 32 * 1024 * 1024;
pub const MAX_JSON: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub size: u64,
}

impl Release {
    pub fn newer_than(&self, current: &str) -> Result<bool, String> {
        if self.draft || self.prerelease {
            return Ok(false);
        }
        let version = self
            .tag_name
            .strip_prefix('v')
            .ok_or("Release tag must start with v")?;
        let version = Version::parse(version).map_err(|e| e.to_string())?;
        if !version.pre.is_empty() || !version.build.is_empty() {
            return Ok(false);
        }
        let current = Version::parse(current).map_err(|e| e.to_string())?;
        if version <= current {
            return Ok(false);
        }
        self.asset_size(MANIFEST, MAX_JSON as u64)?;
        self.asset_size(ARCHIVE, MAX_ARCHIVE)?;
        Ok(true)
    }
    pub fn asset_size(&self, name: &str, maximum: u64) -> Result<u64, String> {
        let matches: Vec<_> = self.assets.iter().filter(|a| a.name == name).collect();
        if matches.len() != 1 || matches[0].size == 0 || matches[0].size > maximum {
            return Err(format!("Release must contain one valid {name} asset"));
        }
        Ok(matches[0].size)
    }
    pub fn asset_url(&self, name: &str) -> String {
        // Only called after newer_than validated the tag as a stable semver.
        format!(
            "https://github.com/{REPOSITORY}/releases/download/{}/{name}",
            self.tag_name
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub version: String,
    pub target: String,
    pub archive: Entry,
    pub files: Vec<Entry>,
}
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Entry {
    fn valid(&self, maximum: u64) -> bool {
        self.size > 0
            && self.size <= maximum
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }
    pub fn verify(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.len() as u64 != self.size || hash(bytes) != self.sha256 {
            return Err(format!("Size or SHA-256 mismatch: {}", self.name));
        }
        Ok(())
    }
}
impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_JSON {
            return Err("Update manifest is too large".into());
        }
        let manifest: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<(), String> {
        let version = Version::parse(&self.version).map_err(|e| e.to_string())?;
        if self.schema != 1
            || self.target != TARGET
            || !version.pre.is_empty()
            || !version.build.is_empty()
            || self.archive.name != ARCHIVE
            || !self.archive.valid(MAX_ARCHIVE)
            || self.files.len() != 2
            || FILES.iter().any(|name| {
                self.files
                    .iter()
                    .filter(|f| f.name == *name && f.valid(MAX_FILE))
                    .count()
                    != 1
            })
        {
            return Err("Unsupported or incomplete update manifest".into());
        }
        Ok(())
    }
    pub fn matches_release(&self, release: &Release) -> Result<(), String> {
        if release.tag_name != format!("v{}", self.version)
            || release.asset_size(ARCHIVE, MAX_ARCHIVE)? != self.archive.size
        {
            return Err("Release and update manifest disagree".into());
        }
        Ok(())
    }
    pub fn verify_directory(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        for entry in &self.files {
            let path = directory.join(&entry.name);
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != entry.size
            {
                return Err(format!("Invalid staged file: {}", entry.name));
            }
            let bytes = fs::read(path).map_err(|e| e.to_string())?;
            entry.verify(&bytes)?;
            verify_pe(&entry.name, &bytes)?;
        }
        Ok(())
    }
}

fn verify_pe(name: &str, bytes: &[u8]) -> Result<(), String> {
    let bad = || format!("Invalid Windows x64 binary: {name}");
    let u16_at = |offset: usize| -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            bytes
                .get(offset..offset + 2)
                .ok_or_else(bad)?
                .try_into()
                .unwrap(),
        ))
    };
    if bytes.get(..2) != Some(b"MZ") {
        return Err(bad());
    }
    let nt =
        u32::from_le_bytes(bytes.get(0x3c..0x40).ok_or_else(bad)?.try_into().unwrap()) as usize;
    if !(0x40..=0x100000).contains(&nt)
        || bytes.get(nt..nt + 4) != Some(b"PE\0\0")
        || u16_at(nt + 4)? != 0x8664
        || u16_at(nt + 24)? != 0x20b
        || (u16_at(nt + 22)? & 0x2000 != 0) != (name == "t7patch.dll")
    {
        return Err(bad());
    }
    if name == "t7patch.dll" {
        super::super::process::export_rva(bytes, b"T7PatchStart")?;
    }
    Ok(())
}

pub fn extract(
    manifest: &Manifest,
    bytes: &[u8],
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    manifest.validate()?;
    manifest.archive.verify(bytes)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    if zip.len() != FILES.len() {
        return Err("Update ZIP must contain exactly the EXE and DLL".into());
    }
    let mut seen = Vec::new();
    for i in 0..zip.len() {
        if cancelled() {
            return Err("Update cancelled".into());
        }
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        let entry = manifest
            .files
            .iter()
            .find(|e| e.name == file.name())
            .ok_or("Unexpected ZIP entry")?;
        if seen.contains(&entry.name)
            || !file.is_file()
            || file.size() != entry.size
            || file
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Duplicate, linked or incorrectly sized ZIP entry".into());
        }
        seen.push(entry.name.clone());
        let mut data = Vec::new();
        (&mut file)
            .take(entry.size + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        entry.verify(&data)?;
        verify_pe(&entry.name, &data)?;
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination.join(&entry.name))
            .map_err(|e| e.to_string())?;
        output
            .write_all(&data)
            .and_then(|_| output.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
