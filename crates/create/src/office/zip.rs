//! Reading the parts of a .docx or .pptx: a ZIP archive (APPNOTE 6.3), stored or deflated
//! entries only. Every size comes from the file, so each is checked against what is there and
//! against a budget: a damaged or hostile archive is refused, never read past its end.

use std::io::Read;

/// Most entries listed (an Office file has tens to a few thousand).
const MAX_ENTRIES: usize = 20_000;
/// Most bytes one part may inflate to.
const MAX_PART: u64 = 128 << 20;

pub(crate) struct Zip<'a> {
    bytes: &'a [u8],
    entries: Vec<Entry>,
}

struct Entry {
    name: String,
    method: u16,
    compressed: u64,
    size: u64,
    /// Offset of the local file header.
    local: u64,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at.checked_add(2)?).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_le_bytes)
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_le_bytes)
}

impl<'a> Zip<'a> {
    /// The archive's directory, or why it can't be read.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self, String> {
        // The end of central directory record: within the last 64 KiB + 22 bytes (its comment).
        let from = bytes.len().saturating_sub(22 + 0xFFFF);
        let eocd = (from..bytes.len().saturating_sub(21))
            .rev()
            .find(|&i| u32_at(bytes, i) == Some(0x0605_4b50))
            .ok_or("not a ZIP archive (no directory)")?;
        let count = usize::from(u16_at(bytes, eocd + 10).ok_or("truncated ZIP directory")?);
        let mut at = usize::try_from(u32_at(bytes, eocd + 16).ok_or("truncated ZIP directory")?).map_err(|_| "ZIP directory out of range")?;
        if count == 0xFFFF || at == 0xFFFF_FFFF {
            return Err("ZIP64 archives are not supported".into());
        }
        let mut entries = Vec::with_capacity(count.min(MAX_ENTRIES));
        for _ in 0..count.min(MAX_ENTRIES) {
            if u32_at(bytes, at) != Some(0x0201_4b50) {
                return Err("damaged ZIP directory".into());
            }
            let field = |off: usize| u32_at(bytes, at + off).map(u64::from).ok_or("truncated ZIP directory");
            let method = u16_at(bytes, at + 10).ok_or("truncated ZIP directory")?;
            let (compressed, size, local) = (field(20)?, field(24)?, field(42)?);
            let name_len = usize::from(u16_at(bytes, at + 28).ok_or("truncated ZIP directory")?);
            let extra = usize::from(u16_at(bytes, at + 30).ok_or("truncated ZIP directory")?);
            let comment = usize::from(u16_at(bytes, at + 32).ok_or("truncated ZIP directory")?);
            let name = bytes.get(at + 46..at + 46 + name_len).ok_or("truncated ZIP directory")?;
            entries.push(Entry { name: String::from_utf8_lossy(name).replace('\\', "/"), method, compressed, size, local });
            at = at.saturating_add(46).saturating_add(name_len).saturating_add(extra).saturating_add(comment);
        }
        Ok(Zip { bytes, entries })
    }

    /// Whether the archive has a part named `name`.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// The bytes of the part named `name` (case-insensitive, as Office reads them).
    pub(crate) fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let name = name.trim_start_matches('/');
        let e = self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("{name} is missing"))?;
        if e.size > MAX_PART {
            return Err(format!("{name} is too large"));
        }
        let local = usize::try_from(e.local).map_err(|_| "ZIP entry out of range")?;
        if u32_at(self.bytes, local) != Some(0x0403_4b50) {
            return Err(format!("{name}: damaged ZIP entry"));
        }
        let name_len = usize::from(u16_at(self.bytes, local + 26).ok_or("truncated ZIP entry")?);
        let extra = usize::from(u16_at(self.bytes, local + 28).ok_or("truncated ZIP entry")?);
        let start = local.saturating_add(30).saturating_add(name_len).saturating_add(extra);
        let len = usize::try_from(e.compressed).map_err(|_| "ZIP entry out of range")?;
        let data = self.bytes.get(start..start.saturating_add(len)).ok_or_else(|| format!("{name}: truncated ZIP entry"))?;
        match e.method {
            0 => Ok(data.to_vec()),
            8 => {
                let mut out = Vec::with_capacity(usize::try_from(e.size.min(MAX_PART)).unwrap_or(0));
                flate2::read::DeflateDecoder::new(data)
                    .take(MAX_PART + 1)
                    .read_to_end(&mut out)
                    .map_err(|err| format!("{name}: damaged compressed data ({err})"))?;
                if out.len() as u64 > MAX_PART {
                    return Err(format!("{name} is too large"));
                }
                Ok(out)
            }
            m => Err(format!("{name}: unsupported ZIP compression {m}")),
        }
    }

    /// The text of an XML part.
    pub(crate) fn text(&self, name: &str) -> Result<String, String> {
        let bytes = self.read(name)?;
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        String::from_utf8(bytes.to_vec()).map_err(|_| format!("{name} is not UTF-8 XML"))
    }
}

/// The target of each relationship in the `.rels` part for `part` (`word/document.xml` →
/// `word/_rels/document.xml.rels`): id → (type, archive path).
pub(crate) fn relationships(zip: &Zip, part: &str) -> std::collections::HashMap<String, (String, String)> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let rels = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
    let mut out = std::collections::HashMap::new();
    let Ok(xml) = zip.text(&rels) else { return out };
    let Ok(tree) = super::xml(&xml) else { return out };
    for r in tree.descendants().filter(|n| n.has_tag_name("Relationship")) {
        let (Some(id), Some(target)) = (r.attribute("Id"), r.attribute("Target")) else { continue };
        if r.attribute("TargetMode") == Some("External") {
            continue;
        }
        let kind = r.attribute("Type").unwrap_or("").rsplit('/').next().unwrap_or("").to_owned();
        out.insert(id.to_owned(), (kind, resolve(dir, target)));
    }
    out
}

/// `target` relative to the folder `dir`, with `..` and `.` resolved; an absolute target
/// (`/ppt/media/a.png`) is from the archive root.
pub(crate) fn resolve(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

#[cfg(test)]
pub(crate) fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
    // A stored-only archive for tests.
    let mut out = Vec::new();
    let mut dir = Vec::new();
    for (name, data) in files {
        let local = out.len() as u32;
        let crc = 0u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        dir.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        dir.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dir.extend_from_slice(&crc.to_le_bytes());
        dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
        dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
        dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
        dir.extend_from_slice(&[0; 12]);
        dir.extend_from_slice(&local.to_le_bytes());
        dir.extend_from_slice(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend_from_slice(&dir);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stored_parts_and_refuses_damage() {
        let z = build(&[("word/document.xml", b"<a/>"), ("x.txt", b"hi")]);
        let zip = Zip::open(&z).unwrap();
        assert!(zip.has("WORD/document.xml"));
        assert_eq!(zip.read("/x.txt").unwrap(), b"hi");
        assert!(zip.read("nope").is_err());
        assert!(Zip::open(b"not a zip").is_err());
        // Every truncation is refused or reads what is there; none panics.
        for cut in 0..z.len() {
            if let Ok(zip) = Zip::open(&z[..cut]) {
                let _ = zip.read("x.txt");
            }
        }
    }

    #[test]
    fn relative_targets_resolve() {
        assert_eq!(resolve("ppt/slides", "../media/image1.png"), "ppt/media/image1.png");
        assert_eq!(resolve("word", "media/a.png"), "word/media/a.png");
        assert_eq!(resolve("word", "/word/media/a.png"), "word/media/a.png");
        assert_eq!(resolve("", "../../x"), "x");
    }
}
