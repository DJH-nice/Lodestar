//! 索引的落盘与读档 —— **冷启动的关键**。
//!
//! 【为什么必须有这一层】
//!   没有它，每次启动应用都要**全量扫描**磁盘：
//!     实测 `$HOME`（8,352 条目）热缓存 74.6ms、冷缓存 300~450ms；
//!     索引到 10 万级就是秒级冷启动。
//!   有了它，启动 = 读一个文件 → **目标 ≤15ms**，扫描只在后台增量做。
//!
//! 【格式】
//!   固定头 + 定长记录 + 字符串。没用 mmap，先用「整读整解析」——
//!   8K 条目约 1.1MB，读+解析在 10ms 以内，已经够用；
//!   mmap 的列存布局是 M2 的优化项（见设计文档 §4.1）。
//!
//! 【为什么写临时文件再 rename】
//!   直接写目标文件的话，中途被杀会留下**半个文件** ——
//!   下次启动读到截断的数据，索引就废了。
//!   rename 是原子的 → 要么是完整的旧档，要么是完整的新档。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::types::IndexEntry;

/// 魔数 —— 认文件用。
const MAGIC: &[u8; 4] = b"SXTX";
/// 格式版本。改了布局就 +1（读档时版本对不上 → 当作没有存档，重新扫）。
const VERSION: u32 = 1;

/// 把索引写到 `path`（原子替换）。
pub fn save(entries: &[IndexEntry], roots: &[PathBuf], path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut buf: Vec<u8> = Vec::with_capacity(entries.len() * 96 + 64);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.extend_from_slice(&(roots.len() as u32).to_le_bytes());
    for r in roots {
        write_str(&mut buf, &r.to_string_lossy());
    }
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        write_str(&mut buf, &e.name);
        write_str(&mut buf, &e.path);
        write_str(&mut buf, &e.dir);
        write_str(&mut buf, &e.extension);
        write_str(&mut buf, &e.folded_name);
        write_str(&mut buf, &e.folded_path);
        buf.extend_from_slice(&e.size.to_le_bytes());
        buf.extend_from_slice(&e.mtime.to_le_bytes());
        let mut flags = 0u8;
        if e.is_folder {
            flags |= 1;
        }
        buf.push(flags);
        buf.extend_from_slice(&e.depth.to_le_bytes());
        buf.extend_from_slice(&e.child_count.to_le_bytes());
    }

    // 临时文件 + 原子 rename
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?; // 落盘再 rename —— 断电也不会留下半个档
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 从 `path` 读索引。**读不出来就返回 None**（调用方退回全量扫描）。
///
/// 【为什么「读不出来」不是错误】
///   没有存档、格式版本变了、文件被截断 —— 这三种都该走「重新扫一遍」，
///   而不是让应用起不来。
pub fn load(path: &Path) -> Option<(Vec<IndexEntry>, Vec<PathBuf>)> {
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
    parse(&buf)
}

/// 解析缓冲区。拆出来是为了能单测「字节流 → 索引」。
fn parse(buf: &[u8]) -> Option<(Vec<IndexEntry>, Vec<PathBuf>)> {
    let mut r = Reader { buf, pos: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    let version = r.u32()?;
    if version != VERSION {
        return None;
    }
    let root_count = r.u32()? as usize;
    let mut roots = Vec::with_capacity(root_count);
    for _ in 0..root_count {
        roots.push(PathBuf::from(r.string()?));
    }
    let entry_count = r.u32()? as usize;
    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let name = r.string()?;
        let path = r.string()?;
        let dir = r.string()?;
        let extension = r.string()?;
        let folded_name = r.string()?;
        let folded_path = r.string()?;
        let size = r.i64()?;
        let mtime = r.i64()?;
        let flags = r.u8()?;
        let depth = r.u32()?;
        let child_count = r.u32()?;
        entries.push(IndexEntry {
            name,
            path,
            dir,
            extension,
            folded_name,
            folded_path,
            size,
            mtime,
            is_folder: flags & 1 != 0,
            depth,
            child_count,
        });
    }
    Some((entries, roots))
}

fn write_str(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn i64(&mut self) -> Option<i64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Some(i64::from_le_bytes(a))
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<IndexEntry> {
        vec![
            IndexEntry {
                name: "报告.pdf".into(),
                path: "/Users/me/Documents/报告.pdf".into(),
                dir: "/Users/me/Documents".into(),
                extension: "pdf".into(),
                size: 12345,
                mtime: 1_700_000_000,
                is_folder: false,
                depth: 2,
                child_count: 0,
                folded_name: "报告.pdf".into(),
                folded_path: "/users/me/documents/报告.pdf".into(),
            },
            IndexEntry {
                name: "子目录".into(),
                path: "/Users/me/Documents/子目录".into(),
                dir: "/Users/me/Documents".into(),
                extension: String::new(),
                size: 0,
                mtime: 1_700_000_001,
                is_folder: true,
                depth: 2,
                child_count: 3,
                folded_name: "子目录".into(),
                folded_path: "/users/me/documents/子目录".into(),
            },
        ]
    }

    #[test]
    fn round_trip_preserves_everything() {
        let entries = sample();
        let roots = vec![PathBuf::from("/Users/me/Documents")];
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&(roots.len() as u32).to_le_bytes());
        for r in &roots {
            write_str(&mut buf, &r.to_string_lossy());
        }
        buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for e in &entries {
            write_str(&mut buf, &e.name);
            write_str(&mut buf, &e.path);
            write_str(&mut buf, &e.dir);
            write_str(&mut buf, &e.extension);
            write_str(&mut buf, &e.folded_name);
            write_str(&mut buf, &e.folded_path);
            buf.extend_from_slice(&e.size.to_le_bytes());
            buf.extend_from_slice(&e.mtime.to_le_bytes());
            buf.push(if e.is_folder { 1 } else { 0 });
            buf.extend_from_slice(&e.depth.to_le_bytes());
            buf.extend_from_slice(&e.child_count.to_le_bytes());
        }

        let (got_entries, got_roots) = parse(&buf).expect("应该能解析");
        assert_eq!(got_roots, roots);
        assert_eq!(got_entries.len(), entries.len());
        for (a, b) in got_entries.iter().zip(entries.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.path, b.path);
            assert_eq!(a.dir, b.dir);
            assert_eq!(a.extension, b.extension);
            assert_eq!(a.folded_name, b.folded_name);
            assert_eq!(a.folded_path, b.folded_path);
            assert_eq!(a.size, b.size);
            assert_eq!(a.mtime, b.mtime);
            assert_eq!(a.is_folder, b.is_folder);
            assert_eq!(a.depth, b.depth);
            assert_eq!(a.child_count, b.child_count);
        }
    }

    #[test]
    fn garbage_input_is_rejected_not_panicked() {
        assert!(parse(b"").is_none());
        assert!(parse(b"XXXX").is_none());
        assert!(parse(b"SXTX").is_none(), "截断的档必须被拒");
        // 版本不对 → 当作没有存档
        let mut buf = Vec::from(MAGIC);
        buf.extend_from_slice(&999u32.to_le_bytes());
        assert!(parse(&buf).is_none());
    }

    #[test]
    fn save_then_load_on_disk() {
        let dir = std::env::temp_dir().join(format!("sextant-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("index.bin");
        let entries = sample();
        let roots = vec![PathBuf::from("/Users/me/Documents")];
        save(&entries, &roots, &path).unwrap();
        let (got, got_roots) = load(&path).expect("应该能读档");
        assert_eq!(got.len(), entries.len());
        assert_eq!(got_roots, roots);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_none() {
        assert!(load(Path::new("/nonexistent/sextant/index.bin")).is_none());
    }
}
