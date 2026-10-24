//! 外接卷的枚举与监控 —— **插上 U 盘就能搜到里面的文件**。
//!
//! 【为什么用系统调用而不是别的】
//!   macOS 的外接盘挂在 `/Volumes/<卷名>`。系统调用能一次列出
//!   **所有**挂载点 + 它们的标志位 —— 一次调用、零依赖、不需要回调。
//!   （`NSFileManager` 要 objc 绑定；`Disk Arbitration` 是回调式的，
//!   要挂 RunLoop —— 对「每秒轮询」这个形态都不划算。）
//!
//! 【为什么是 `getfsstat` 而不是 `getmntinfo`】
//!   这两个在 macOS 上**语义不一样**（在 FreeBSD 上才是一家人），照抄会崩：
//!     - `getmntinfo(&buf, flags)`：把数组写进**库自己的静态对象**，
//!       只把指针给你（man 3 getmntinfo 的 BUGS 段原话）。
//!       → 传 NULL 去问个数会**直接段错误**（它无条件往 `mntbufp` 写）；
//!         而且那个数组**不是你的** —— 别的线程再调一次 `getmntinfo` 就会把它改了
//!         （man page 明说它 not thread-safe）。
//!     - `getfsstat(buf, len, flags)`：**缓冲区是自己的**，
//!       `buf = NULL` 时只回「有多少个挂载点」（man 2 getfsstat 明写）。
//!   → 用 `getfsstat`：两段式、内存归自己、没有静态缓冲区可被别人踩。
//!   （2026-10-24 实测：照 FreeBSD 的写法调 `getmntinfo(NULL, …)` → SIGSEGV。）

use std::path::{Path, PathBuf};

/// 挂载点的标志位：本地磁盘（网络盘 smbfs/nfs **没有**这个位）。
///
/// 【为什么靠它排除网络盘】
///   网络共享也挂在 `/Volumes/` 下（`/Volumes/公司共享`）——
///   用户说的是「插硬盘」，网络盘不在范围内，而且网络盘扫描慢、容易断。
const MNT_LOCAL: u32 = libc::MNT_LOCAL as u32;

/// 当前挂载的**本地外接卷**。
///
/// 【判据（三条全中才算）】
///   1. `MNT_LOCAL` —— 本地磁盘（排除 smbfs / nfs / afpfs / webdav）
///   2. 挂载点是 `/Volumes` 的**直接子目录**（排除 `/` 与 `/System/Volumes/*`）
///   3. 不是 `/Volumes/Macintosh HD` 那个指向 `/` 的符号链接
///      —— 这一条**不用写代码判**：内核的挂载表里根本没有它。
///      （2026-10-24 实测：根卷在表里的路径是 `/`；
///      `/Volumes/Macintosh HD` 只是系统摆在那里的**符号链接**，从来不是挂载点。）
///      → 判据 2 顺带就把它挡住了。
pub fn mounted_local_volumes() -> Vec<PathBuf> {
    // 【getfsstat 的两段式调用】第一次只问「有多少个」，
    // 第二次才把内容填进我们给的数组里。
    //
    // 【为什么第一次可以传 NULL】
    //   man 2 getfsstat：「If buf is given as NULL, getfsstat() returns just
    //   the number of mounted file systems.」→ 传 NULL 时它只回个数，不碰内存。
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count <= 0 {
        return Vec::new();
    }
    // 【这个 Vec 为什么是安全的】
    //   长度 = 刚才问来的个数；元素是 `statfs` —— 纯 POD（全零是合法位型），
    //   内核只往里写前 `count` 个，不会越界。
    let mut buf: Vec<libc::statfs> = unsafe { vec![std::mem::zeroed(); count as usize] };
    // 【为什么字节数要这么算】
    //   `getfsstat` 的第二个参数是**字节数**，不是元素个数 ——
    //   写小了只填进前几个，写大了就可能越界。
    let bytes = std::mem::size_of::<libc::statfs>() * count as usize;
    // 【第二次调用为什么安全】
    //   `buf.as_mut_ptr()` 指向**我们自己的** Vec，
    //   它的长度正好是 `count` 个 `statfs`、字节数就是上面那个 `bytes` ——
    //   内核最多写这么多，**不可能越界**。
    //   `MNT_NOWAIT`：直接用内核里缓存的挂载信息，不等各个文件系统回应 ——
    //   正在轮询，一个不肯回应的网络盘不能把整个轮询拖住。
    let filled = unsafe {
        libc::getfsstat(buf.as_mut_ptr(), bytes as libc::c_int, libc::MNT_NOWAIT)
    };
    // 【为什么要 clamp】挂载点数量只可能**变少**（内核按缓冲区大小截断），
    //   但万一是负数（出错）就直接当空 —— 绝不让它去切片越界。
    let filled = filled.clamp(0, count) as usize;
    let mounts: Vec<(PathBuf, u32)> = buf[..filled]
        .iter()
        .map(|s| {
            // 【为什么用 CStr 读原始字节】这里要的是挂载点**本身**，
            // 不是它指向的东西 —— 内核填进来的是 NUL 结尾的 C 字符串
            // （`f_mntonname` 是 `[c_char; 1024]`，内核保证 NUL 结尾）。
            let name = unsafe { std::ffi::CStr::from_ptr(s.f_mntonname.as_ptr()) };
            (
                PathBuf::from(name.to_string_lossy().into_owned()),
                s.f_flags,
            )
        })
        .collect();
    filter_local_volumes(&mounts)
}

/// 从挂载点里挑出「本地外接卷」—— **纯函数**（好测）。
pub fn filter_local_volumes(mounts: &[(PathBuf, u32)]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = mounts
        .iter()
        .filter(|(_, flags)| flags & MNT_LOCAL != 0)
        .filter(|(p, _)| p.parent() == Some(Path::new("/Volumes")))
        .map(|(p, _)| p.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_volume_is_not_an_external_volume() {
        let mounts = vec![(PathBuf::from("/"), MNT_LOCAL)];
        assert!(filter_local_volumes(&mounts).is_empty(), "根卷不是外接盘");
    }

    #[test]
    fn system_volumes_are_not_external_volumes() {
        let mounts = vec![
            (PathBuf::from("/System/Volumes/Data"), MNT_LOCAL),
            (PathBuf::from("/System/Volumes/Preboot"), MNT_LOCAL),
        ];
        assert!(filter_local_volumes(&mounts).is_empty());
    }

    #[test]
    fn a_usb_stick_under_volumes_is_picked_up() {
        let mounts = vec![(PathBuf::from("/Volumes/U盘"), MNT_LOCAL)];
        assert_eq!(
            filter_local_volumes(&mounts),
            vec![PathBuf::from("/Volumes/U盘")]
        );
    }

    #[test]
    fn network_shares_are_skipped() {
        // 网络盘的标志位里**没有** MNT_LOCAL
        let mounts = vec![(PathBuf::from("/Volumes/公司共享"), 0)];
        assert!(filter_local_volumes(&mounts).is_empty(), "网络盘不自动索引");
    }

    #[test]
    fn nested_paths_under_volumes_are_not_picked_up() {
        let mounts = vec![(PathBuf::from("/Volumes/U盘/子目录"), MNT_LOCAL)];
        assert!(
            filter_local_volumes(&mounts).is_empty(),
            "只认 /Volumes 的直接子目录"
        );
    }

    #[test]
    fn the_result_is_sorted_and_deduplicated() {
        let mounts = vec![
            (PathBuf::from("/Volumes/B"), MNT_LOCAL),
            (PathBuf::from("/Volumes/A"), MNT_LOCAL),
            (PathBuf::from("/Volumes/A"), MNT_LOCAL),
        ];
        assert_eq!(
            filter_local_volumes(&mounts),
            vec![PathBuf::from("/Volumes/A"), PathBuf::from("/Volumes/B")]
        );
    }

    /// 真机冒烟：**真的**调一次系统调用。
    ///
    /// 【为什么必须有这条】
    ///   `filter_local_volumes` 是纯函数 —— 它**测不到**「取挂载表」那一段。
    ///   而那一段最容易出的错不是「结果不对」，是**段错误**：
    ///   macOS 的 `getmntinfo` 与 FreeBSD 语义不同（传 NULL 问个数 → SIGSEGV），
    ///   照着 FreeBSD 的写法抄一遍 → 整个测试进程直接崩，
    ///   **别的测试连报都来不及报**。
    ///   → 「纯函数单测全绿」≠「真的调得通系统调用」。
    #[test]
    fn the_real_system_call_does_not_crash() {
        let vols = mounted_local_volumes();
        // 不管这台机器插没插 U 盘，结果都得满足这几条
        for v in &vols {
            assert!(v.is_absolute(), "{:?} 不是绝对路径", v);
            assert_eq!(
                v.parent(),
                Some(Path::new("/Volumes")),
                "{:?} 不在 /Volumes 的直接子目录上",
                v
            );
        }
        // 规矩跟纯函数一致：排序 + 去重
        let mut sorted = vols.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(vols, sorted);
    }
}
