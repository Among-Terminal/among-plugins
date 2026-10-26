use crate::Result;
use flate2::{bufread::GzDecoder, Compression, GzBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

pub const MAX_FILE: u64 = 64 * 1024 * 1024;
pub type Files = BTreeMap<String, (Vec<u8>, bool)>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').count() <= 16
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.starts_with(".among-")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        })
}

pub fn read(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(format!("not a bounded regular file: {}", path.display()).into());
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err("file grew beyond limit".into());
    }
    Ok(bytes)
}

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}

pub fn pack(path: &Path, files: &Files) -> Result<Vec<Member>> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let gzip = GzBuilder::new()
        .mtime(0)
        .write(file, Compression::default());
    let mut tar = tar::Builder::new(gzip);
    let mut members = Vec::new();
    for (path, (bytes, executable)) in files {
        if !relative(path) {
            return Err(format!("unsafe archive path: {path}").into());
        }
        let mut header = tar::Header::new_ustar();
        header.set_path(path)?;
        header.set_size(bytes.len() as u64);
        header.set_mode(if *executable { 0o755 } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append(&header, bytes.as_slice())?;
        members.push(Member {
            path: path.clone(),
            size: bytes.len() as u64,
            sha256: hash(bytes),
            executable: *executable,
        });
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(members)
}

pub fn unpack(bytes: &[u8], expected: &[Member]) -> Result<BTreeMap<String, Vec<u8>>> {
    if bytes.len() as u64 > MAX_FILE || expected.is_empty() || expected.len() > 2048 {
        return Err("archive bounds exceeded".into());
    }
    let mut gzip = GzDecoder::new(bytes);
    let mut expanded = Vec::new();
    gzip.by_ref()
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut expanded)?;
    if expanded.len() > 256 * 1024 * 1024 || !gzip.into_inner().is_empty() {
        return Err("expanded bound or trailing gzip stream".into());
    }
    let mut declared = BTreeMap::new();
    for member in expected {
        if !relative(&member.path)
            || member.size > MAX_FILE
            || declared.insert(member.path.as_str(), member).is_some()
        {
            return Err("invalid or duplicate member declaration".into());
        }
    }
    let mut result = BTreeMap::new();
    let mut archive = tar::Archive::new(expanded.as_slice());
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            return Err("non-regular package member".into());
        }
        let path = entry.path()?.to_str().ok_or("non-UTF-8 member")?.to_owned();
        let member = declared
            .get(path.as_str())
            .ok_or("unexpected package member")?;
        if result.contains_key(&path)
            || entry.size() != member.size
            || entry.header().mode()? != if member.executable { 0o755 } else { 0o644 }
        {
            return Err("member size, mode or uniqueness mismatch".into());
        }
        let mut content = Vec::new();
        entry
            .by_ref()
            .take(MAX_FILE + 1)
            .read_to_end(&mut content)?;
        if content.len() as u64 != member.size || hash(&content) != member.sha256 {
            return Err("member digest mismatch".into());
        }
        result.insert(path, content);
    }
    let consumed = archive.into_inner().len();
    if !expanded[expanded.len() - consumed..]
        .iter()
        .all(|byte| *byte == 0)
        || result.len() != declared.len()
    {
        return Err("missing members or trailing tar data".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_unsafe_paths() {
        for path in [
            "/bin/x",
            "../x",
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            ".among-private",
        ] {
            assert!(!relative(path));
        }
        assert!(relative("bin/clock-managed"));
    }

    #[test]
    fn actual_archives_round_trip_and_reject_corruption_missing_and_trailing_data() {
        let directory =
            std::env::temp_dir().join(format!("among-package-archive-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("package.tar.gz");
        let files = BTreeMap::from([
            ("among-plugin.json".into(), (b"{}\n".to_vec(), false)),
            ("bin/clock-managed".into(), (b"binary".to_vec(), true)),
        ]);
        let members = pack(&path, &files).unwrap();
        let bytes = read(&path, MAX_FILE).unwrap();
        let unpacked = unpack(&bytes, &members).unwrap();
        assert_eq!(unpacked["bin/clock-managed"], b"binary");
        let mut corrupt = bytes.clone();
        corrupt[10] ^= 1;
        assert!(unpack(&corrupt, &members).is_err());
        assert!(unpack(&bytes[..bytes.len() - 2], &members).is_err());
        assert!(unpack(&bytes, &members[..1]).is_err());
        let mut missing = members.clone();
        missing.push(Member {
            path: "missing".into(),
            size: 1,
            sha256: hash(b"x"),
            executable: false,
        });
        assert!(unpack(&bytes, &missing).is_err());
        let mut duplicate = members.clone();
        duplicate.push(members[0].clone());
        assert!(unpack(&bytes, &duplicate).is_err());
        let mut trailing = bytes.clone();
        trailing.extend_from_slice(&bytes);
        assert!(unpack(&trailing, &members).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
