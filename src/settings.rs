//! File format shared by the launcher and the DLL, with atomic publication of edits.
use std::{
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::Path,
};
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub name: Vec<u8>,
    pub friends_only: bool,
    pub password: Vec<u8>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            name: b"Unknown Soldier".to_vec(),
            friends_only: true,
            password: Vec::new(),
        }
    }
}
impl Config {
    pub fn parse(&mut self, input: &[u8]) {
        for line in input.split(|&b| b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Some(index) = line.iter().position(|&b| b == b'=') else {
                continue;
            };
            let value = &line[index + 1..];
            if value.contains(&0) {
                continue;
            }
            match &line[..index] {
                b"playername" => self.name = value[..value.len().min(15)].to_vec(),
                b"networkpassword" => self.password = value[..value.len().min(1023)].to_vec(),
                b"isfriendsonly" => {
                    self.friends_only = std::str::from_utf8(value)
                        .ok()
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .unwrap_or(0)
                        != 0
                }
                _ => {}
            }
        }
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut config = Self::default();
        config.parse(&std::fs::read(path)?);
        Ok(config)
    }
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if self.name.len() > 15
            || self.password.len() > 1023
            || self
                .name
                .iter()
                .chain(&self.password)
                .any(|b| [0, b'\n', b'\r'].contains(b))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Name: maximum 15 UTF-8 bytes. Password: maximum 1023 bytes. No line breaks.",
            ));
        }
        let mut bytes = b"playername=".to_vec();
        bytes.extend_from_slice(&self.name);
        bytes.extend_from_slice(if self.friends_only {
            b"\nisfriendsonly=1\nnetworkpassword="
        } else {
            b"\nisfriendsonly=0\nnetworkpassword="
        });
        bytes.extend_from_slice(&self.password);
        bytes.push(b'\n');
        Ok(bytes)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = self.encode()?;
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing config directory")
        })?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = parent.join(format!(".t7patch-{}-{stamp}.tmp", std::process::id()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            let from: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            if unsafe {
                MoveFileExW(
                    from.as_ptr(),
                    to.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn format_roundtrip_and_invalid_edits() {
        let c = Config {
            name: b"Rust".to_vec(),
            friends_only: false,
            password: b"a=b".to_vec(),
        };
        let mut parsed = Config::default();
        parsed.parse(&c.encode().unwrap());
        assert_eq!(c, parsed);
        let mut invalid = c.clone();
        invalid.name = vec![b'x'; 16];
        assert!(invalid.encode().is_err());
        invalid = c;
        invalid.password = b"one\nplayername=two".to_vec();
        assert!(invalid.encode().is_err());
    }

    #[test]
    fn replace_file_and_keep_previous_on_invalid_edit() {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Directory(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!("settings-test-{}-{stamp}", std::process::id())),
        );
        std::fs::create_dir_all(&directory.0).unwrap();
        let path = directory.0.join("config-ñ.conf");
        let mut config = Config::default();
        config.save(&path).unwrap();
        config.name = "Rusté".as_bytes().to_vec();
        config.password = b"a=b".to_vec();
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
        let previous = std::fs::read(&path).unwrap();
        config.name = vec![b'x'; 16];
        assert!(config.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), previous);
    }
}
