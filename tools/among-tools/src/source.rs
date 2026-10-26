use crate::{archive, Result};
use std::fs;
use std::path::Path;

pub struct Collector {
    pub files: archive::Files,
    bytes: u64,
    entries: usize,
    maximum_files: usize,
    maximum_bytes: u64,
}

impl Collector {
    pub fn new(maximum_files: usize, maximum_bytes: u64) -> Self {
        Self {
            files: archive::Files::new(),
            bytes: 0,
            entries: 0,
            maximum_files,
            maximum_bytes,
        }
    }

    fn remaining(&self, path: &str) -> Result<u64> {
        if !archive::relative(path)
            || self.files.contains_key(path)
            || self.files.len() >= self.maximum_files
        {
            return Err(format!("source path, duplicate or file-count limit: {path}").into());
        }
        self.maximum_bytes
            .checked_sub(self.bytes)
            .ok_or_else(|| "source byte budget exhausted".into())
    }

    pub fn insert(&mut self, path: String, bytes: Vec<u8>) -> Result<()> {
        let remaining = self.remaining(&path)?;
        if bytes.len() as u64 > remaining {
            return Err("source byte budget exhausted".into());
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or("source byte count overflow")?;
        self.files.insert(path, (bytes, false));
        Ok(())
    }

    pub fn file(&mut self, path: String, source: &Path) -> Result<()> {
        let remaining = self.remaining(&path)?.min(archive::MAX_FILE);
        // Size is admitted before reading. The bounded read also refuses growth
        // beyond remaining credit before retaining any new file in the map.
        let bytes = archive::read(source, remaining)?;
        self.insert(path, bytes)
    }

    pub fn text_file(&mut self, path: String, source: &Path) -> Result<()> {
        self.file(path.clone(), source)?;
        let bytes = &self
            .files
            .get(&path)
            .ok_or("text input was not collected")?
            .0;
        if std::str::from_utf8(bytes).map_or(true, |text| text.trim().is_empty()) {
            return Err(format!("empty or non-UTF-8 required text input: {path}").into());
        }
        Ok(())
    }

    pub fn walk(&mut self, root: &Path, prefix: &str, depth: usize) -> Result<()> {
        if depth > 16 {
            return Err("source directory depth exceeded".into());
        }
        for entry in fs::read_dir(root)? {
            self.entries = self
                .entries
                .checked_add(1)
                .filter(|count| *count <= 65536)
                .ok_or("source entry count exceeded")?;
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF-8 source name")?;
            let path = format!("{prefix}/{name}");
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.walk(&entry.path(), &path, depth + 1)?;
            } else if kind.is_file() {
                self.file(path, &entry.path())?;
            } else {
                return Err("linked or special source file".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_text_collection_rejects_empty_or_whitespace_files() {
        let directory =
            std::env::temp_dir().join(format!("among-required-text-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("LICENSE");
        for bytes in [b"".as_slice(), b" \n\t".as_slice(), &[0xff]] {
            fs::write(&path, bytes).unwrap();
            let mut collector = Collector::new(4, 1024);
            assert!(collector
                .text_file("LICENSE".into(), &path)
                .unwrap_err()
                .to_string()
                .contains("empty or non-UTF-8"));
        }
        fs::write(&path, b"licence text").unwrap();
        assert!(Collector::new(4, 1024)
            .text_file("LICENSE".into(), &path)
            .is_ok());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn running_byte_and_flat_directory_limits_apply_before_retention() {
        let directory =
            std::env::temp_dir().join(format!("among-source-budget-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(directory.join(name), b"1234").unwrap();
        }
        let mut collector = Collector::new(2, 8);
        assert!(collector.walk(&directory, "vendor", 0).is_err());
        assert_eq!(collector.files.len(), 2);
        assert_eq!(collector.bytes, 8);
        let mut collector = Collector::new(8, 7);
        assert!(collector.walk(&directory, "vendor", 0).is_err());
        assert_eq!(collector.files.len(), 1);
        assert_eq!(collector.bytes, 4);
        assert!(collector
            .file("tracked/b".into(), &directory.join("b"))
            .is_err());
        assert_eq!(collector.files.len(), 1);
        collector.bytes = u64::MAX;
        assert!(collector
            .file("tracked/c".into(), &directory.join("c"))
            .is_err());
        for name in ["a", "b", "c"] {
            fs::remove_file(directory.join(name)).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }
}
