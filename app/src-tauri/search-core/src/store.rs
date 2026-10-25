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
//!   现在只有一种布局：**v3**（**每根一个档**）——
//!   为了「插拔外接卷时，只有卷自己的档作废，$HOME 的档不受影响」。
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

/// 格式版本 —— **v3**：**每根一个档**。
///
/// 【v3】改成**每个根一个档** —— 档里不再有 roots 列表，只有「自己这个根」。
///   为什么要拆：插拔一次外接卷 → roots 集合变了 → v2 的「roots 全等校验」
///   会把**整份档**（包括 $HOME 的）判成「不是这份索引」→ $HOME 全量重扫。
///   拆开之后，卷的档与 $HOME 的档**互不影响**。
///   v2 的老档直接作废 → 一次性全量重扫（可接受）。
const VERSION: u32 = 3;

// ───────────────────────── 条目读写 ─────────────────────────
//
// 【为什么条目部分要单独成对】`write_entry` / `read_entry` 是**一对互逆的实现**
//   —— 抄两遍（每个格式一份）的话，将来加一个字段要记得改两处 ——
//   漏一处就会写出「字段静默全是 0」的档，比读不出来还难查。
//   现在只剩 v3 一种格式了 → 这一对由 v3 的**逐字节 golden**（见测试）钉住。

/// 写一条条目。
fn write_entry(buf: &mut Vec<u8>, e: &IndexEntry) {
    write_str(buf, &e.name);
    write_str(buf, &e.path);
    write_str(buf, &e.dir);
    write_str(buf, &e.extension);
    write_str(buf, &e.folded_name);
    write_str(buf, &e.folded_path);
    buf.extend_from_slice(&e.size.to_le_bytes());
    buf.extend_from_slice(&e.mtime.to_le_bytes());
    let mut flags = 0u8;
    if e.is_folder {
        flags |= 1;
    }
    buf.push(flags);
    buf.extend_from_slice(&e.depth.to_le_bytes());
    buf.extend_from_slice(&e.child_count.to_le_bytes());
    buf.extend_from_slice(&e.child_file_count.to_le_bytes());
    buf.extend_from_slice(&e.child_folder_count.to_le_bytes());
}

/// 所有条目的字节数（**不含头**）—— 落盘前的容量估算。
///
/// 【为什么要抽出来】`serialize_root` 要先算「这份档大概多大」
///   → 一次分配、零拷贝。抄一份到别处的话，那份迟早会漏字段 —— 漏一个字段
///   就会写出「字段静默全是 0」的档，比读不出来还难查。
///
/// 【必须与 `write_entry` 逐字段对齐】这里算的每个字段，
///   `write_entry` 都必须真的写出去。**估小了**只是多一次扩容；
///   **估大了**是白占内存。真正要防的是两边**不一致**——
///   所以下面每一项都要跟 `write_entry` 里的字段一一对得上。
fn entries_bytes(entries: &[IndexEntry]) -> usize {
    let mut total = 0usize;
    for e in entries {
        // 6 个字符串：每个是「4 字节长度前缀 + 内容」（`write_str` 的写法）
        total += 4 + e.name.len();
        total += 4 + e.path.len();
        total += 4 + e.dir.len();
        total += 4 + e.extension.len();
        total += 4 + e.folded_name.len();
        total += 4 + e.folded_path.len();
        // size(8) + mtime(8) + flags(1) + depth(4)
        // + child_count(4) + child_file_count(4) + child_folder_count(4)
        total += 8 + 8 + 1 + 4 + 4 + 4 + 4;
    }
    total
}

/// 读一条条目（`write_entry` 的逆操作）。
fn read_entry(r: &mut Reader<'_>) -> Option<IndexEntry> {
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
    let child_file_count = r.u32()?;
    let child_folder_count = r.u32()?;
    Some(IndexEntry {
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
        child_file_count,
        child_folder_count,
    })
}

/// 把**已经序列化好的**字节流写到 `path`（原子替换）。
///
/// 【为什么要单独抽出来】落盘有两段成本：
///   **序列化**（CPU，约 1ms）与 **写盘**（I/O，约 12ms —— 新建 inode 的
///   fsync 很贵）。第二段可以挪到后台线程去做 → 关键路径上只留第一段。
///
/// 【`sync` 参数】`true` = 落盘再 rename（断电也不会留下半个档）。
///   `false` = 不 fsync —— 断电时这个档可能坏掉，
///   但**读档失败会退回全量扫描**（见 [`load_root`]），所以只是慢，不会出错。
pub fn save_bytes(buf: &[u8], path: &Path, sync: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 临时文件 + 原子 rename
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(buf)?;
        if sync {
            f.sync_all()?; // 落盘再 rename —— 断电也不会留下半个档
        }
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ───────────────────────── v3：每根一个档 ─────────────────────────

/// 根路径 → 它的档在 `dir` 里的文件名。
///
/// 【为什么用哈希而不是把路径编码进文件名】
///   路径里有 `/` 和中文，长度也不封顶 → 文件名会不合法或超长。
///   哈希是定长十六进制 → 永远是合法文件名。
///
/// 【碰撞了会怎样】
///   哈希吃的是**原始字节**（见 [`fnv1a64`]）→ 字节不同的根**几乎总是**
///   落到不同的档文件（FNV-1a **不是单射** —— 理论上可被构造出碰撞，
///   概率极低，但不是零）。
///   万一撞了，档里**记着自己的根路径**（见 [`parse_root`]）→ 读出来对不上
///   就当没有档（退回全量扫描）→ 最多浪费一次扫描，**不会读错数据**。
pub fn root_archive_path(dir: &Path, root: &Path) -> PathBuf {
    dir.join(format!(
        "{:016x}.bin",
        fnv1a64(root.as_os_str().as_encoded_bytes())
    ))
}

/// FNV-1a 64 位 —— 自己写，不依赖 `DefaultHasher`
/// （后者的种子不保证跨 Rust 版本稳定，不能拿来当持久化文件名）。
///
/// 【为什么吃原始字节，不吃 lossy 字符串】
///   lossy 会把**非法 UTF-8 字节**统一替换成 U+FFFD →
///   两个**字节不同**的路径会算出**同一个哈希**、落到**同一个档文件**上，
///   而档里记的 lossy 路径又「对得上」→ **读到别的根的档**。
///   吃原始字节就**基本**解决了这个问题：字节不同的路径**几乎总是**落到
///   不同的档文件（FNV-1a **不是单射** —— 碰撞理论上可被构造出来，
///   概率极低，但不是零）。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 把**一个根**的条目序列化成字节流。
///
/// 【档的布局】MAGIC + 版本 + **这个根的路径** + 这个根的条目。
///   （v2 时代是「一份档装所有根」—— 已被 v3 取代，理由见 [`VERSION`]。）
pub fn serialize_root(root: &Path, entries: &[IndexEntry]) -> Vec<u8> {
    // 【为什么按头部实际字节算，别靠一个固定的常数蒙】
    //   头 = 魔数(4) + 版本(4) + 根路径的长度前缀(4) + 根路径 + 条目数前缀(4)
    //       = 16 + 根路径长度。
    //   拿固定的 64 蒙的话，根路径**超过 48 字节**就会蒙穿 → 多一次 Vec 扩容
    //   （整 buf memcpy，量级 0.1~1ms，每次 save 一次）。
    //   条目部分继续用 `entries_bytes`（与 `write_entry` 同一份字段表）。
    let root_str = root.to_string_lossy();
    let cap = 16 + root_str.len() + entries_bytes(entries);
    let mut buf: Vec<u8> = Vec::with_capacity(cap);
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    write_str(&mut buf, &root_str);
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in entries {
        write_entry(&mut buf, e);
    }
    buf
}

/// 把一个根的条目写到 `dir` 里它自己的档（原子替换）。
pub fn save_root(dir: &Path, root: &Path, entries: &[IndexEntry]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = root_archive_path(dir, root);
    save_bytes(&serialize_root(root, entries), &path, true)
}

/// 读某个根的档。**读不出来就返回 `None`**（调用方退回全量扫描）。
///
/// 【为什么「读不出来」不是错误】
///   没有档、格式版本变了、文件被截断、哈希碰撞对不上根 ——
///   这四种都该走「重新扫一遍」，而不是让应用起不来。
pub fn load_root(dir: &Path, root: &Path) -> Option<Vec<IndexEntry>> {
    let path = root_archive_path(dir, root);
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.read_to_end(&mut buf).ok()?;
    let (saved_root, entries) = parse_root(&buf)?;
    if saved_root != root.to_string_lossy() {
        // 哈希碰撞 → 这份档不是这个根的 → 当没有档
        return None;
    }
    Some(entries)
}

/// 一条条目的**最小**字节数（字符串内容长度可以为 0）。
///
/// 【怎么算出来的】6 个字符串的长度前缀（6 × 4 = 24）
///   + size(8) + mtime(8) + flags(1) + depth(4)
///   + child_count(4) + child_file_count(4) + child_folder_count(4) = **57**。
///   （这跟 `write_entry` / `entries_bytes` 是同一份字段表 —— 三处必须一致。）
const MIN_ENTRY_BYTES: usize = 57;

/// 档里声称的条目数，这段剩余字节**装不装得下**。
///
/// 【为什么要有它】档里的条目数是 u32 → 最大 42.9 亿。直接拿它去
///   `Vec::with_capacity` → 分配失败会**直接 abort**（而不是返回 `None`），
///   违反「读不出来 → 重扫」的承诺；在开了 overcommit 的系统上还会先占掉
///   几百 GB 虚拟内存。坏档应该走「重新扫一遍」，不是让应用起不来。
///
/// 【怎么判】一条条目最少也要 [`MIN_ENTRY_BYTES`] 字节
///   → 装不下就说明这份档在说谎 → 当没有档。
fn count_fits(buf: &[u8], pos: usize, count: usize) -> bool {
    let remaining = buf.len().saturating_sub(pos);
    count <= remaining / MIN_ENTRY_BYTES
}

/// 解析 v3 档 → (根路径, 条目)。
fn parse_root(buf: &[u8]) -> Option<(String, Vec<IndexEntry>)> {
    let mut r = Reader { buf, pos: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    if r.u32()? != VERSION {
        return None;
    }
    let root = r.string()?;
    let count = r.u32()? as usize;
    // 档里的条目数不可信 → 先看剩下的字节装不装得下（见 `count_fits`）。
    if !count_fits(r.buf, r.pos, count) {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(read_entry(&mut r)?);
    }
    Some((root, entries))
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
                child_file_count: 0,
                child_folder_count: 0,
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
                child_file_count: 2,
                child_folder_count: 1,
                folded_name: "子目录".into(),
                folded_path: "/users/me/documents/子目录".into(),
            },
        ]
    }

    #[test]
    fn round_trip_preserves_everything() {
        let entries = sample();
        let root = PathBuf::from("/Users/me/Documents");
        let buf = serialize_root(&root, &entries);

        let (got_root, got_entries) = parse_root(&buf).expect("应该能解析");
        assert_eq!(got_root, root.to_string_lossy(), "根路径要原样回来");
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
            assert_eq!(a.child_file_count, b.child_file_count);
            assert_eq!(a.child_folder_count, b.child_folder_count);
        }
    }

    #[test]
    fn garbage_input_is_rejected_not_panicked() {
        assert!(parse_root(b"").is_none());
        assert!(parse_root(b"XXXX").is_none());
        assert!(parse_root(b"SXTX").is_none(), "截断的档必须被拒");
        // 版本不对 → 当作没有存档
        let mut buf = Vec::from(MAGIC);
        buf.extend_from_slice(&999u32.to_le_bytes());
        assert!(parse_root(&buf).is_none());
    }

    #[test]
    fn an_older_archive_version_is_rejected() {
        // 老档（v2）布局不同（头里是 roots 列表）→ 版本对不上 → 当作没有存档
        //   → 重新扫一次。
        // 【为什么要钉住】静默读半截老档 = 子文件数/子目录数全是 0 →
        //   `childfilecount:` 会给出一批**看着正常、其实全错**的结果。
        let mut buf = Vec::from(MAGIC);
        buf.extend_from_slice(&(VERSION - 1).to_le_bytes());
        assert!(
            parse_root(&buf).is_none(),
            "旧版本档案必须被拒 —— 否则会读出一份字段缺失的索引"
        );
    }

    #[test]
    fn a_root_archive_round_trips() {
        let dir = std::env::temp_dir().join(format!("lodestar-store-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = PathBuf::from("/Volumes/U盘");
        let entries = sample();
        save_root(&dir, &root, &entries).expect("应该能写档");
        let got = load_root(&dir, &root).expect("应该能读档");
        assert_eq!(got.len(), entries.len());
        assert_eq!(got[0].path, entries[0].path);
        assert_eq!(got[0].name, entries[0].name);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_roots_never_share_an_archive() {
        let dir = std::env::temp_dir().join(format!("lodestar-store-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = PathBuf::from("/Users/me");
        let usb = PathBuf::from("/Volumes/U盘");
        save_root(&dir, &home, &sample()).expect("应该能写档");
        assert!(
            load_root(&dir, &usb).is_none(),
            "另一个根不该读到这份档 —— 这正是「插拔卷不作废 $HOME 的档」的前提"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_archive_name_is_stable_per_root() {
        let dir = Path::new("/tmp/archives");
        let a = root_archive_path(dir, Path::new("/Users/me"));
        let b = root_archive_path(dir, Path::new("/Volumes/U盘"));
        assert_eq!(a, root_archive_path(dir, Path::new("/Users/me")), "同一个根 → 同一个文件名");
        assert_ne!(a, b, "不同的根 → 不同的文件名");
        assert_eq!(a.extension().unwrap(), "bin");
    }

    #[test]
    fn a_missing_archive_returns_none() {
        assert!(load_root(Path::new("/nonexistent/lodestar"), Path::new("/Users/me")).is_none());
    }

    #[test]
    fn a_truncated_root_archive_is_rejected() {
        // 【为什么要钉住】`load_root` 的文档承诺「文件被截断 → 当没有档 →
        //   重新扫一遍」。截断的档如果被读成「少几条的档」，那就会拿一份
        //   **残缺的索引**当完整的用 —— 比「读不出来」危险得多。
        let buf = serialize_root(Path::new("/Users/me"), &sample());
        assert!(parse_root(&buf[..buf.len() - 1]).is_none(), "截断的档必须被拒");
    }

    #[test]
    fn paths_with_the_same_lossy_form_get_different_archives() {
        // 两个路径的 lossy 形式相同（都含非法 UTF-8 字节 → 都被替换成 U+FFFD），
        // 但原始字节不同 → 必须落到**不同的档文件**（否则会读到别的根的档）。
        let dir = std::env::temp_dir().join(format!("lodestar-store-lossy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        use std::os::unix::ffi::OsStrExt;
        let a = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/Volumes/\xff\xfe"));
        let b = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/Volumes/\xff\xfd"));
        assert_eq!(
            a.to_string_lossy(),
            b.to_string_lossy(),
            "前提：这两个路径的 lossy 形式必须相同"
        );
        assert_ne!(
            root_archive_path(&dir, &a),
            root_archive_path(&dir, &b),
            "原始字节不同 → 必须落到不同的档文件"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ───────────────────── 落盘布局的规格（golden） ─────────────────────

    /// 一条**手写**的条目字节期望 —— v3 落盘布局的**规格**。
    ///
    /// 【为什么测试里要手写一份】
    ///   `write_entry` / `read_entry` 是**一对互逆的实现** → 只测它们互相能读通，
    ///   证明不了「落盘的字节布局没变」。而落盘布局是**跨版本契约**
    ///   （用户磁盘上已经有老档）：将来有人给 `write_entry` 加了字段却忘了
    ///   bump 版本号 → 两个 helper 一起变 → 只测互逆性的测试**照样全绿**，
    ///   而**老档会被静默读成错值**。
    ///   所以这里手写一份**期望字节**（连 4 字节长度前缀都用**字面量**写死，
    ///   不调 `write_str`）→ 布局一变立刻变红。
    ///
    /// 【覆盖了全部 13 个字段】name / path / dir / extension / folded_name /
    ///   folded_path / size / mtime / is_folder / depth / child_count /
    ///   child_file_count / child_folder_count。
    fn golden_entry() -> (IndexEntry, Vec<u8>) {
        let e = IndexEntry {
            name: "sub".into(),
            path: "/r/sub".into(),
            dir: "/r".into(),
            extension: String::new(),
            folded_name: "sub".into(),
            folded_path: "/r/sub".into(),
            size: 11,
            mtime: 12,
            is_folder: true,
            depth: 1,
            child_count: 2,
            child_file_count: 3,
            child_folder_count: 4,
        };
        let mut b: Vec<u8> = Vec::new();
        // name（3 字节）+ 4 字节长度前缀
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(b"sub");
        // path（6 字节）
        b.extend_from_slice(&6u32.to_le_bytes());
        b.extend_from_slice(b"/r/sub");
        // dir（2 字节）
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"/r");
        // extension（空字符串 → 只有 4 字节长度前缀）
        b.extend_from_slice(&0u32.to_le_bytes());
        // folded_name（3 字节）
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(b"sub");
        // folded_path（6 字节）
        b.extend_from_slice(&6u32.to_le_bytes());
        b.extend_from_slice(b"/r/sub");
        // size(8) + mtime(8)
        b.extend_from_slice(&11i64.to_le_bytes());
        b.extend_from_slice(&12i64.to_le_bytes());
        // flags（is_folder → 1）
        b.push(1);
        // depth(4) + child_count(4) + child_file_count(4) + child_folder_count(4)
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&4u32.to_le_bytes());
        (e, b)
    }

    #[test]
    fn the_v3_entry_layout_is_byte_for_byte_stable() {
        let (e, expected_entry) = golden_entry();
        let buf = serialize_root(Path::new("/r"), std::slice::from_ref(&e));

        // 条目部分：与手写期望逐字节相同
        let at = buf.len() - expected_entry.len();
        assert_eq!(
            &buf[at..],
            expected_entry.as_slice(),
            "v3 的条目字节布局必须逐字节不变（跨版本契约）"
        );

        // 整份档（头 + 条目）：逐字节相同
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b"SXTX");
        expected.extend_from_slice(&3u32.to_le_bytes());
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(b"/r");
        expected.extend_from_slice(&1u32.to_le_bytes());
        expected.extend_from_slice(&expected_entry);
        assert_eq!(buf, expected, "v3 的落盘字节必须逐字节不变");

        // 容量估算必须与 `write_entry` 实际写出的字节数一致 ——
        //   这两个数一旦漂移（有人给 write_entry 加字段却忘了改估算），
        //   就要在这里被抓住，而不是等到「多一次 Vec 扩容」才被发现。
        assert_eq!(
            entries_bytes(std::slice::from_ref(&e)),
            expected_entry.len(),
            "容量估算必须与 write_entry 写出的字节数一致"
        );
    }

    #[test]
    fn the_minimum_entry_size_matches_the_sanity_check() {
        // 【为什么要钉住】`count_fits` 拿「剩余字节 ÷ 单条最小字节数」判
        //   「这份档在说谎」。这个最小值必须**恰好**等于一条空条目的字节数 ——
        //   估大了 → 明明装得下的档被误判成坏的（白扫一遍）；
        //   估小了 → 坏档能骗过检查（又回到「分配失败直接 abort」）。
        let e = IndexEntry {
            name: String::new(),
            path: String::new(),
            dir: String::new(),
            extension: String::new(),
            folded_name: String::new(),
            folded_path: String::new(),
            size: 0,
            mtime: 0,
            is_folder: false,
            depth: 0,
            child_count: 0,
            child_file_count: 0,
            child_folder_count: 0,
        };
        assert_eq!(
            entries_bytes(&[e]),
            MIN_ENTRY_BYTES,
            "单条条目的最小字节数必须与 entries_bytes 一致"
        );
    }

    #[test]
    fn the_capacity_estimate_covers_the_whole_archive() {
        // 【为什么要钉住】`serialize_root` 的容量估算必须**按头实际占的字节算**
        //   （魔数 4 + 版本 4 + 根路径长度前缀 4 + 根路径 + 条目数前缀 4
        //     = 16 + 根路径长度）。
        //   靠一个固定的常数蒙的话，根路径**超过 48 字节**就会蒙穿
        //   → 多一次 Vec 扩容（整 buf memcpy，0.1~1ms，每次 save 一次）。
        //   这里断言「估算公式 == 实际产出的字节数」→ 公式与实现绑死。
        let root = Path::new("/this/index/root/path/is/definitely/longer/than/forty/eight/bytes");
        let entries = sample();
        let buf = serialize_root(root, &entries);
        // 估算公式 == 实际产出的字节数
        assert_eq!(
            buf.len(),
            16 + root.to_string_lossy().len() + entries_bytes(&entries),
            "容量估算公式必须与实际产出的字节数一致"
        );
        // **一次分配就够** → 没发生扩容。
        //   估算蒙穿时 Vec 会被撑大（容量翻倍）→ 这里立刻变红。
        assert_eq!(
            buf.capacity(),
            buf.len(),
            "容量估算必须一次就够，不该发生 Vec 扩容"
        );
    }

    #[test]
    fn a_hash_collision_is_detected_and_the_archive_is_rejected() {
        // 【为什么要钉住】这是全局约束「档里记着自己的根路径 → 碰撞能检出」
        //   的**唯一实现**（`load_root` 里那句 `saved_root != root.to_string_lossy()`）。
        //   其它测试走的都是「文件不存在」分支 → 覆盖不到它。
        //   以后被改坏（例如那句比对被删掉）不会有任何测试变红。
        let dir = std::env::temp_dir().join(format!("lodestar-store-collide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = PathBuf::from("/Volumes/U盘");
        let other = PathBuf::from("/Volumes/别的盘");

        // 造一份「档里记的是**别的根**」的 v3 字节流
        let bytes = serialize_root(&other, &sample());

        // 前提校验：这份字节流本身是**好的** —— 能解析出「别的根」。
        //   少了这一步，「load_root 返回 None」可能只是因为字节流坏了
        //   → 就没测到「根对不上」那条分支。
        let parsed = parse_root(&bytes).expect("前提：这份字节流必须是好的");
        assert_eq!(parsed.0, other.to_string_lossy());

        // 把它写进**请求的根**该在的那个文件（= 模拟哈希碰撞）
        save_bytes(&bytes, &root_archive_path(&dir, &root), true).expect("应该能写档");

        assert!(
            load_root(&dir, &root).is_none(),
            "档里记的是别的根 → 必须当没有档（重扫），不能读到别人的条目"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_that_claims_too_many_entries_is_rejected() {
        // 【为什么要钉住】档里的条目数是 u32 → 坏档可以随便写一个 42.9 亿。
        //   直接拿它 `Vec::with_capacity` → 分配失败会**直接 abort**
        //   （而不是返回 None）→ 一个坏档能把应用**炸掉**。
        //   坏档应该走「读不出来 → 重扫」，不是让应用起不来。
        let mut buf = Vec::from(MAGIC);
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(b"/r");
        buf.extend_from_slice(&u32::MAX.to_le_bytes()); // 条目数：42.9 亿
        assert!(parse_root(&buf).is_none(), "在说谎的档必须被拒");
    }

    #[test]
    fn the_entry_count_gate_rejects_what_does_not_fit() {
        // 【为什么要有这条】上面那条端到端测试走的是「分支被走到」，
        //   但**抓不住「闸门被删掉」** —— 因为 macOS 开着 overcommit，
        //   `Vec::with_capacity(42.9 亿)` 那 790GB **虚拟内存**居然申请成功，
        //   于是「有闸门」与「没闸门」两种情况**结果都是 None**。
        //   所以这里直接测**闸门本身的算术**（白盒）——
        //   公式被改坏（比如把 57 写成 61、或把比较号反过来）立刻变红。
        //   剩下「闸门有没有被调用」由上面那条端到端测试覆盖分支。
        // 装不下：剩 0 字节，却说有 42.9 亿条
        assert!(!count_fits(&[0u8; 16], 16, u32::MAX as usize));
        // 装不下：剩 56 字节（差 1 字节就够一条）却说有 1 条
        assert!(!count_fits(&[0u8; 56], 0, 1));
        // 刚好装下：57 字节 = 1 条
        assert!(count_fits(&[0u8; 57], 0, 1));
        // 空档 + 0 条 → 合法
        assert!(count_fits(&[0u8; 16], 16, 0));
    }
}
