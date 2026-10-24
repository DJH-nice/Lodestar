//! 外接卷的枚举与监控 —— **插上 U 盘就能搜到里面的文件**。
//!
//! 【为什么用系统调用而不是别的】
//!   macOS 的外接盘挂在 `/Volumes/<卷名>`。系统调用 `getfsstat` 能一次列出
//!   **所有**挂载点 + 它们的标志位 —— 一次调用、零依赖、不需要回调。
//!   （`NSFileManager` 要 objc 绑定；`Disk Arbitration` 是回调式的，
//!   要挂 RunLoop —— 对「每秒轮询」这个形态都不划算。）
//!
//!   ⚠️ **为什么不是 `getmntinfo`** —— 那是 **FreeBSD 语义**，在 macOS 上**必崩**：
//!   ① 第一次调用传 NULL 问个数 → 直接 **SIGSEGV**（实测复现）；
//!   ② 它把结果写进**库自己的静态对象**（不是我们的缓冲区）→ 不是线程安全的。
//!   `getfsstat` 是**两段式、缓冲区归调用方**：第一次问个数、第二次填我们自己的
//!   `Vec` → 下面那段 unsafe 的安全理由**字面成立**。
//!
//! 【失败为什么要走 `Err` 而不是「空列表」】
//!   调用方（Task 5 的监控循环）靠这个函数的**集合变化**判断卷出现/消失。
//!   系统调用失败**也是**空列表 → 会被误判成「所有卷都被拔了」
//!   → 触发不必要的拆索引 + 全量重扫（U 盘白扫一遍，十几秒）。
//!   → 失败走 `Err`（带上 errno）+ 一条 warn 日志；
//!     调用方**这一轮什么都不做**。

use std::path::{Path, PathBuf};

/// 挂载点的标志位：本地磁盘（网络盘 smbfs/nfs **没有**这个位）。
///
/// 【为什么靠它排除网络盘】
///   网络共享也挂在 `/Volumes/` 下（`/Volumes/公司共享`）——
///   用户说的是「插硬盘」，网络盘不在范围内，而且网络盘扫描慢、容易断。
const MNT_LOCAL: u32 = libc::MNT_LOCAL as u32;

/// 根卷在 `/Volumes` 下的**符号链接** —— 绝不能当成外接卷。
///
/// 【为什么必须显式挡它】
///   macOS 把 `/Volumes/Macintosh HD` 做成指向 `/` 的**符号链接**。
///   今天的内核挂载表里**没有**这个路径（根卷在表里的路径是 `/`，
///   源设备是 `/dev/disk3s1s1` —— 2026-10-24 实测），
///   但那是**平台碰巧如此**，**没有一行代码保证**：
///   `/Volumes/Macintosh HD` 恰恰**就是** `/Volumes` 的直接子目录
///   → 判据 2（父目录必须是 `/Volumes`）**挡不住**它。
///   → 一旦哪天 macOS（或 Target Disk Mode 挂载）把它列进挂载表，
///     应用会把**根卷 `/` 当成 U 盘**整个索引一遍。
///   → 显式挡，不靠平台碰巧。
const ROOT_VOLUME_ALIAS: &str = "/Volumes/Macintosh HD";

/// 当前挂载的**本地外接卷**。
///
/// 【判据（四条全中才算）】
///   1. `MNT_LOCAL` —— 本地磁盘（排除 smbfs / nfs / afpfs / webdav）
///   2. 挂载点是 `/Volumes` 的**直接子目录**（排除 `/` 与 `/System/Volumes/*`）
///   3. **不是** `/Volumes/Macintosh HD`（根卷的符号链接）
///      —— 这一条**必须显式判**：它**也是** `/Volumes` 的直接子目录，
///      判据 2 **挡不住**它（详见 `ROOT_VOLUME_ALIAS`）。
///
/// 【`Err` 是什么意思】
///   「**这一轮的挂载表没拿到**」—— **不是**「没有外接卷」。
///   调用方**必须**把 `Err` 与「空列表」分开看（见文件头那段）。
pub fn mounted_local_volumes() -> Result<Vec<PathBuf>, String> {
    match enumerate_local_volumes() {
        Ok((vols, _filled, _requested)) => Ok(vols),
        Err(e) => {
            // 【为什么失败还要打日志】
            //   `Err` 只告诉调用方「这一轮别动」，但**排查**
            //   「为什么这一轮没动」只有日志能回答（errno 就在里面）。
            crate::logging::log_line(&format!("[volumes] 枚举外接卷失败：{e}"));
            Err(e)
        }
    }
}

/// 内部：真正干活的那一段 —— **顺带把两个数带出来**给测试看。
///
/// 【为什么要把「填了几个槽 / 要了几个槽」带出来】
///   内核只会往缓冲区里**装得下**那么多个（实测：1 槽缓冲区 → 返回 1，
///   而总挂载数是 11 —— 见 man 2 getfsstat）。
///   → 字节数算小了，挂载表**最后一条**会被悄悄丢掉
///     （本机最后一条是 `/System/Volumes/Update/mnt1`）。
///   → 而丢掉的那条**不在** `/Volumes` 下 → 只看**过滤后**的结果，
///     所有断言照样全绿 → 光靠返回值**发现不了**截断。
///
/// 【返回】`(外接卷列表, 内核填了几个槽, 我们要了几个槽)`。
fn enumerate_local_volumes() -> Result<(Vec<PathBuf>, usize, usize), String> {
    // ① 问「有多少个挂载点」
    let count = mount_point_count();
    if count <= 0 {
        // 【为什么 0 也当失败】
        //   macOS 上「一个挂载点都没有」不可能出现（根卷至少在）→
        //   真出现 = 这个返回值**不可信** → 当失败处理，
        //   保守起见让调用方**这一轮什么都不做** ——
        //   总好过被误判成「所有卷都被拔了」。
        return Err(format!(
            "getfsstat 问个数失败（errno={:?}）",
            std::io::Error::last_os_error()
        ));
    }
    let requested = count as usize;
    // ② 分配**我们自己的**缓冲区
    //   （`statfs` 是纯 POD —— 全零是合法位型，内核只往里写前 `requested` 个。）
    let mut buf: Vec<libc::statfs> = unsafe { vec![std::mem::zeroed(); requested] };
    // 【为什么字节数要 checked 算 + checked 窄化】
    //   `getfsstat` 的第二个参数是**字节数**（不是元素个数）。
    //   裸的 `as libc::c_int` 是**无检查窄化** → 字节数一旦超过 `i32::MAX`
    //   就悄悄变负（今天要约 99 万个挂载点才够得到，
    //   但这是典型的「以后某天会炸」的形状）→ 宁可报错也不要静默出错。
    let bytes = std::mem::size_of::<libc::statfs>()
        .checked_mul(requested)
        .and_then(|b| libc::c_int::try_from(b).ok())
        .ok_or_else(|| {
            format!(
                "挂载表太大：{} 个挂载点 × {} 字节装不进一个 i32",
                requested,
                std::mem::size_of::<libc::statfs>()
            )
        })?;
    // ③ 取内容
    // 【为什么这次调用安全】
    //   `buf.as_mut_ptr()` 指向**我们自己的** Vec：它装得下 `requested` 个
    //   `statfs`、字节数就是上面那个 `bytes` → 内核最多写这么多，**不可能越界**。
    //   `MNT_NOWAIT`：直接用内核里缓存的挂载信息，不等各个文件系统回应 ——
    //   正在轮询，一个不肯回应的网络盘不能把整个轮询拖住。
    let filled = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if filled < 0 {
        return Err(format!(
            "getfsstat 取内容失败（errno={:?}）",
            std::io::Error::last_os_error()
        ));
    }
    // 【clamp 的真实理由】
    //   内核返回的是**它实际拷贝进来的个数**（缓冲区装不下就只拷前几个）
    //   → 返回值**恒 ≤** 缓冲区能装的个数 → 上界那半在 macOS 上**不可达**
    //     （纯防御性，不是「正常路径」）。
    //   下界那半是给「返回负数」兜底 —— 切片前下标必须一定合法。
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
    Ok((filter_local_volumes(&mounts), filled, requested))
}

/// 现在有多少个挂载点。
///
/// 【为什么单独抽出来】
///   unsafe 只出现在这一个地方（macOS 的 `getfsstat` 是 3 参数签名：
///   `getfsstat(buf, 字节数, flags)` —— BSD/Linux 那边才是 2 参数的
///   `getfsstat64(buf, &mut bufsize)`，照抄会编译不过）。
///   测试也要用它做交叉核对 → 抽出来，测试里就不用再写一遍 unsafe。
fn mount_point_count() -> i32 {
    // 【为什么这里传 NULL 是安全的】
    //   man 2 getfsstat：「If buf is given as NULL, getfsstat() returns just
    //   the number of mounted file systems.」→ 它只回个数，不碰内存。
    unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) }
}

/// 从挂载点里挑出「本地外接卷」—— **纯函数**（好测）。
pub fn filter_local_volumes(mounts: &[(PathBuf, u32)]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = mounts
        .iter()
        .filter(|(_, flags)| flags & MNT_LOCAL != 0)
        .filter(|(p, _)| p.parent() == Some(Path::new("/Volumes")))
        // 【判据 3：显式挡根卷的符号链接】
        //   `/Volumes/Macintosh HD` **也是** `/Volumes` 的直接子目录
        //   → 上一条（父目录必须是 `/Volumes`）**挡不住**它 → 必须单独判。
        .filter(|(p, _)| p != Path::new(ROOT_VOLUME_ALIAS))
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
    fn the_root_volume_alias_under_volumes_is_not_picked_up() {
        // 评审给的**反例**：`/Volumes/Macintosh HD` 是根卷的符号链接，
        // 而它**恰恰是** `/Volumes` 的直接子目录 → 判据 2 挡不住它。
        let mounts = vec![(PathBuf::from("/Volumes/Macintosh HD"), MNT_LOCAL)];
        assert!(
            filter_local_volumes(&mounts).is_empty(),
            "根卷的符号链接不能当成 U 盘"
        );
    }

    #[test]
    fn the_volumes_directory_itself_is_not_a_volume() {
        let mounts = vec![(PathBuf::from("/Volumes"), MNT_LOCAL)];
        assert!(filter_local_volumes(&mounts).is_empty());
    }

    #[test]
    fn an_empty_mount_table_yields_no_volumes() {
        assert!(filter_local_volumes(&[]).is_empty());
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
        let (vols, filled, requested) = enumerate_local_volumes().expect("枚举外接卷失败");
        // 【为什么断言「内核填了几个槽」】
        //   内核只会往缓冲区里**装得下**那么多个（实测：1 槽缓冲区 → 返回 1，
        //   而总挂载数是 11）→ 字节数算小了，挂载表**最后一条**会被悄悄丢掉，
        //   而那条通常**不在** `/Volumes` 下（本机是 `/System/Volumes/Update/mnt1`）
        //   → 只看**过滤后**的结果，所有断言照样全绿 → 截断发现不了。
        //   → 这里断言「一个都没被丢」。
        //   （挂载数在这两次调用之间**可能变**——正好这时插了个 U 盘：
        //     那时 filled < requested 是**正常**的，不是截断
        //     → 所以再问一次个数，没变才敢断言。）
        let after = mount_point_count();
        if after == requested as i32 {
            assert_eq!(
                filled, requested,
                "挂载表被截断了：要 {} 个挂载点，只装进 {} 个 → 挂载表最后一条被丢了",
                requested, filled
            );
        }
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
        // 【为什么还要过一遍公开入口】
        //   公开入口多包了一层 `Result` —— 那层包装本身也得有人走一遍
        //   （这台机器上挂载表是拿得到的 → 必须是 `Ok`，
        //     而且结果要和内部枚举**一致**）。
        let from_public = mounted_local_volumes().expect("公开入口也该成功");
        assert_eq!(from_public, vols, "公开入口与内部枚举结果不一致");
    }
}
