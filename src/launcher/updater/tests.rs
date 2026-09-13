use super::{http, install, package::*};
use std::{
    fs,
    io::{Cursor, Write},
    path::PathBuf,
};

pub(super) struct Directory(pub PathBuf);
static NEXT_DIRECTORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
impl Directory {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "t7patch-update-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_DIRECTORY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn pe(dll: bool) -> Vec<u8> {
    let mut b = vec![0u8; 2048];
    fn w16(b: &mut [u8], o: usize, v: u16) {
        b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn w32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    w16(&mut b, 0, 0x5a4d);
    w32(&mut b, 0x3c, 0x80);
    w32(&mut b, 0x80, 0x4550);
    w16(&mut b, 0x84, 0x8664);
    w16(&mut b, 0x86, 1);
    w16(&mut b, 0x94, 240);
    w16(&mut b, 0x96, if dll { 0x2022 } else { 0x22 });
    w16(&mut b, 0x98, 0x20b);
    w32(&mut b, 0x98 + 60, 0x200);
    w32(&mut b, 0x98 + 112, 0x1000);
    w32(&mut b, 0x98 + 116, 0x100);
    let s = 0x98 + 240;
    w32(&mut b, s + 8, 0x600);
    w32(&mut b, s + 12, 0x1000);
    w32(&mut b, s + 16, 0x600);
    w32(&mut b, s + 20, 0x200);
    w32(&mut b, s + 36, 0x60000000);
    w32(&mut b, 0x214, 1);
    w32(&mut b, 0x218, 1);
    w32(&mut b, 0x21c, 0x1048);
    w32(&mut b, 0x220, 0x1040);
    w32(&mut b, 0x224, 0x1050);
    w32(&mut b, 0x240, 0x1060);
    w32(&mut b, 0x248, 0x1200);
    b[0x260..0x26d].copy_from_slice(b"T7PatchStart\0");
    b
}
pub(super) fn archive(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
pub(super) fn fixture() -> (Manifest, Vec<u8>) {
    let entries = [(FILES[0], pe(false)), (FILES[1], pe(true))];
    let bytes = archive(&entries);
    let manifest = Manifest {
        schema: 1,
        version: "9.0.0".into(),
        target: TARGET.into(),
        archive: Entry {
            name: ARCHIVE.into(),
            size: bytes.len() as u64,
            sha256: hash(&bytes),
        },
        files: entries
            .iter()
            .map(|(name, b)| Entry {
                name: (*name).into(),
                size: b.len() as u64,
                sha256: hash(b),
            })
            .collect(),
    };
    (manifest, bytes)
}
pub(super) fn stage(directory: &Directory) -> PathBuf {
    let (manifest, archive) = fixture();
    let guard = install::create_stage(&directory.0).unwrap();
    extract(&manifest, &archive, &guard.path.join("new"), || false).unwrap();
    install::write_new(
        &guard.path.join(MANIFEST),
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    guard.persist()
}

#[test]
fn release_versions_and_required_assets() {
    let mut release = Release {
        tag_name: "v0.10.0".into(),
        draft: false,
        prerelease: false,
        assets: vec![
            Asset {
                name: ARCHIVE.into(),
                size: 100,
            },
            Asset {
                name: MANIFEST.into(),
                size: 200,
            },
        ],
    };
    assert!(release.newer_than("0.2.0").unwrap());
    assert!(!release.newer_than("0.10.0").unwrap());
    assert!(!release.newer_than("1.0.0").unwrap());
    release.prerelease = true;
    assert!(!release.newer_than("0.1.0").unwrap());
    release.prerelease = false;
    release.tag_name = "v0.11.0-rc.1".into();
    assert!(!release.newer_than("0.1.0").unwrap());
    release.tag_name = "v1.0.0".into();
    release.assets.push(Asset {
        name: ARCHIVE.into(),
        size: 100,
    });
    assert!(release.newer_than("0.1.0").is_err());
}

#[test]
fn package_verifies_both_binaries_and_preserves_config() {
    let dir = Directory::new();
    let (manifest, bytes) = fixture();
    fs::write(dir.0.join("t7patch.conf"), b"playername=Test").unwrap();
    extract(&manifest, &bytes, &dir.0, || false).unwrap();
    manifest.verify_directory(&dir.0).unwrap();
    assert_eq!(
        fs::read(dir.0.join("t7patch.conf")).unwrap(),
        b"playername=Test"
    );
    fs::write(dir.0.join("t7patch.dll"), pe(false)).unwrap();
    assert!(manifest.verify_directory(&dir.0).is_err());
}

#[test]
fn rejects_corruption_unexpected_zip_paths_and_cancellation() {
    let dir = Directory::new();
    let (mut manifest, mut bytes) = fixture();
    bytes[0] ^= 1;
    assert!(extract(&manifest, &bytes, &dir.0, || false).is_err());
    let malicious = archive(&[("../t7patch.conf", pe(false)), (FILES[1], pe(true))]);
    manifest.archive.size = malicious.len() as u64;
    manifest.archive.sha256 = hash(&malicious);
    assert!(extract(&manifest, &malicious, &dir.0, || false).is_err());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    let (manifest, bytes) = fixture();
    assert!(extract(&manifest, &bytes, &dir.0, || true).is_err());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[test]
fn rejects_incomplete_manifest_wrong_version_and_wrong_architecture() {
    let (mut manifest, _) = fixture();
    let release = Release {
        tag_name: "v8.0.0".into(),
        draft: false,
        prerelease: false,
        assets: vec![Asset {
            name: ARCHIVE.into(),
            size: manifest.archive.size,
        }],
    };
    assert!(manifest.matches_release(&release).is_err());
    manifest.files.pop();
    assert!(manifest.validate().is_err());
    let (mut manifest, _) = fixture();
    let mut wrong = pe(false);
    wrong[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
    let bytes = archive(&[(FILES[0], wrong.clone()), (FILES[1], pe(true))]);
    manifest.files[0].sha256 = hash(&wrong);
    manifest.archive.size = bytes.len() as u64;
    manifest.archive.sha256 = hash(&bytes);
    assert!(extract(&manifest, &bytes, &Directory::new().0, || false).is_err());
}

#[test]
fn download_urls_require_github_https_hosts() {
    for url in [
        "http://github.com/a",
        "https://github.com.evil.test/a",
        "https://user@github.com/a",
        "https://github.com:443/a",
        "https://github.com/a\r\nHeader:x",
        "https://example.com/a",
    ] {
        assert!(http::split_url(url).is_err(), "{url}");
    }
    assert!(
        http::split_url("https://release-assets.githubusercontent.com/path?sig=abc%2Fdef").is_ok()
    );
}

#[test]
#[ignore = "manual network transport smoke test"]
fn github_update_endpoint_smoke() {
    let stop = std::sync::atomic::AtomicBool::new(false);
    let (status, body) = http::get(
        &format!("https://api.github.com/repos/{REPOSITORY}/releases/latest"),
        MAX_JSON,
        &stop,
        |_| {},
    )
    .unwrap();
    assert!([200, 404, 403, 429].contains(&status), "HTTP {status}");
    if status == 200 {
        serde_json::from_slice::<Release>(&body).unwrap();
    }
    println!("Native WinHTTP GitHub update endpoint: HTTP {status}");
}
