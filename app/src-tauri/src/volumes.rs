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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime};

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
        //
        // 【为什么不打 errno】`getfsstat` 返回 0 **不设置 errno** →
        //   `last_os_error()` 拿到的是**上一次**系统调用留下的值 →
        //   消息会渲染成「errno=Undefined error: 0」，
        //   与「errno=…」这个字面承诺相悖（挂起项 #7）。
        //   → 这一支就不提 errno 了。
        return Err("getfsstat 问个数失败：返回 0".to_string());
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
        // 【判据 4：命中排除规则名字的卷整个跳过】
        //   一个叫 `Library`（或 `.隐藏`）的外接卷 → 扫描时**整棵树**被剪掉
        //   → 0 条目 → 状态栏显示「Library · 就绪 · 0 项」，用户完全不知道为什么。
        //   → 与其显示一个「空盘」，不如**不索引它**。
        //
        //   ⚠️ **排除规则只有一份真相**（`search_core::exclude`）——
        //     这里**不抄第二份规则**，直接问它那个既有入口。
        .filter(|(p, _)| !is_excluded_volume(p))
        .map(|(p, _)| p.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 这个挂载点是不是「命中排除规则的卷」（整个盘都不该索引）。
///
/// 【为什么用 `exclude` 的既有入口】排除规则只有一份真相（全局约束 C10）——
///   抄一份到 Rust 侧就会漂移（改了扫描器忘了改这里 → 又开始空转）。
///   `path_is_excluded` 连**祖先**一起查 —— 正好是我们要的语义
///   （卷名 `Library` → 命中；卷名 `U盘` → 不命中）。
fn is_excluded_volume(mount: &Path) -> bool {
    search_core::exclude::path_is_excluded(mount)
}

// ============================================================
// 监控与编排 —— 「插上 U 盘就能搜到里面的文件」
// ============================================================

/// 每隔多久枚举一次挂载点。
///
/// 【为什么是 1 秒】`getfsstat` 的开销在几十微秒量级（见设计文档 §4）；
///   1 秒的延迟对「插上盘」这个动作来说完全可以接受
///   （人插完盘总还要点一下搜索框）。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 「外接卷变化了」事件 —— 载荷是 `IndexStatus`。
///
/// 【为什么需要它】
///   前端靠 800ms 轮询里的「scanning 从 true 翻到 false」触发重搜。
///   卷的附加/分离**不会**让 `scanning` 翻转 → 轮询撞不出来 →
///   结果表里留着已经拔掉的盘的条目（用户最不能接受的那种：盘都拔了还搜得到）。
///   → 必须显式发事件。
///
/// 【这一行的两端都接上了】（Task 6 落地后的状态）
///   ① 本常量**已经**在 `event_names::EVENT_NAMES` 表里 → 有护栏盯着；
///   ② 前端**已经**在 `app/src/App.vue` 里
///      `listen("index-volumes-changed", …)`（**字面量** ——
///      那道护栏断言的是「事件名出现在含 listen 的那一行上」）。
///   → 有人删掉前端的订阅 → 护栏立刻红（不会再出现「发了没人听」）。
pub const VOLUMES_CHANGED_EVENT: &str = "index-volumes-changed";

/// 一个卷现在的状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VolumeStatus {
    /// 挂载点路径（如 `/Volumes/U盘`）。
    pub path: String,
    /// 卷名（挂载点的最后一段）。
    pub label: String,
    /// 处在哪一步。
    pub phase: VolumePhase,
    /// **这个卷自己**的条目数。
    ///
    /// ⚠️ **不是整个引擎的条目总数** —— 那个数是 `IndexState.indexed` 的事。
    ///   以前两处用的是同一个数（`attach_root` 返回的全局总数）→
    ///   $HOME 8,000 项 + U 盘 150 个文件 → 状态栏显示「U盘 · 8,150 项」。
    ///   → 这个数字是错的，而它是用户唯一能看到的「盘上到底有多少东西」的指标。
    pub indexed: u32,
}

/// 卷的状态机。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "state", content = "detail")]
pub enum VolumePhase {
    /// 正在扫（第一次插上，或插回来后的后台刷新）。
    Indexing,
    /// 就绪。
    Ready,
    /// 出错了（reason 是原因）。
    Failed(String),
}

/// 卷状态 —— Tauri 管理的全局状态。
///
/// 【为什么单独一个状态而不是塞进 `IndexState`】
///   `IndexState` 讲的是「引擎索引了什么」；
///   这里讲的是「哪些外接卷插着、扫到哪一步了」—— 两件事，分开放。
#[derive(Default)]
pub struct VolumeState {
    inner: Mutex<Vec<VolumeStatus>>,
}

impl VolumeState {
    /// 当前所有卷的状态（给状态栏看）。
    pub fn snapshot(&self) -> Vec<VolumeStatus> {
        self.inner.lock().unwrap().clone()
    }

    fn set(&self, path: &Path, phase: VolumePhase, indexed: u32) {
        let mut g = self.inner.lock().unwrap();
        let label = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match g.iter_mut().find(|v| v.path == path.to_string_lossy()) {
            Some(v) => {
                v.phase = phase;
                v.indexed = indexed;
            }
            None => g.push(VolumeStatus {
                path: path.to_string_lossy().into_owned(),
                label,
                phase,
                indexed,
            }),
        }
    }

    fn remove(&self, path: &Path) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|v| v.path != path.to_string_lossy());
    }
}

/// 比较「上次知道的」与「现在的」→ (新出现的, 消失的)。
///
/// 【为什么差分而不是直接对着当前集合做】
///   差分能明确回答两件事：**谁来了**（要附加）、**谁走了**（要分离）。
///   直接对当前集合做的话，「走了」这件事就没人管了。
pub fn diff_volumes(
    known: &HashSet<PathBuf>,
    now: &HashSet<PathBuf>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut appeared: Vec<PathBuf> = now.difference(known).cloned().collect();
    let mut disappeared: Vec<PathBuf> = known.difference(now).cloned().collect();
    appeared.sort();
    disappeared.sort();
    (appeared, disappeared)
}

// ============================================================
// 附加编排 —— 「插回来秒开 + 后台刷新」两段
// ============================================================

/// 一次附加里，**这是第几段**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachPhase {
    /// **秒开**：挂的是旧索引档里的条目 —— 挂上去之后**还要**后台刷新一遍。
    Instant,
    /// **最后一段**：挂的是刚扫出来的条目 —— 挂上去就绪了。
    Final,
}

/// 挂上一批条目之后的两个数 —— **语义不同，不能混用**。
///
/// 【为什么必须分开】`attach_root` 返回的是**整个引擎**的条目总数（含 $HOME）。
///   以前把它直接填进 `VolumeStatus.indexed` → 状态栏显示「U盘 · 8,150 项」
///   （$HOME 8,000 + U 盘 150）→ **这个数字是错的**。
///   → 卷自己的条数（状态栏）与引擎的总数（索引状态）从一开始就分开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attached {
    /// **这个卷自己**的条目数 → 状态栏显示的是它。
    pub own: u32,
    /// 挂上之后**整个引擎**的条目数 → `IndexState.indexed` 用它。
    pub total: u32,
}

/// 附加没成功 —— **三种，处理方式不同**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachError {
    /// 引擎还没装好 → 下一轮就该再试（**不贵**，没扫过盘）。
    EngineNotReady,
    /// 盘已经被拔掉 → **不是故障** → 把状态清掉（不留幽灵行）。
    Vanished,
    /// 真出错（扫描失败 / 引擎拒绝附加）→ 状态标 Failed，过一会儿再试。
    Failed(String),
}

/// 附加的**结果** —— 成功，或上面那三种失败之一。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachResult {
    /// 挂上了。
    Ok,
    /// 没挂上（看里面是哪种失败）。
    Error(AttachError),
}

/// 一次卷附加跑完之后的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeAttachOutcome {
    /// 没有旧档：只有一段（首次索引）。
    Indexed {
        /// 这个卷自己的条目数。
        own: u32,
    },
    /// 命中旧档：两段（秒开 + 刷新）。
    Refreshed {
        /// 秒开时挂上去的条目数（旧档里的）。
        cached_own: u32,
        /// 刷新后挂上去的条目数（刚扫出来的）。
        fresh_own: u32,
    },
}

/// 一次卷附加的**编排** —— 「秒开」与「后台刷新」两段。
///
/// 【为什么要抽成一个不依赖 Tauri 的纯编排】
///   「插回来立刻能搜 + 后台刷一遍」是本特性**承诺的一半**
///   （设计文档 §6 M5 验收 #3：「重新插入 → 1s 内可搜到（旧索引复用）→ 随后后台刷新完成」）。
///   第一版实现把「命中旧索引档」写成了**早退** → 承诺的那一半**根本没实现**，
///   而且是**静默**的：插上刚在别的机器上拷过文件的 U 盘 → 搜不到那个文件 →
///   没有报错、状态栏还显示「U盘 · N 项 · 就绪」，而且**不自愈**
///   （FSEvents 不会补发拔插期间的历史事件）。
///   → 这里把它抽成纯编排 + 一条护栏测试 → 下次重构也删不掉它。
///
/// 【两段的顺序不能反】
///   先挂旧档（毫秒级 → 用户马上能搜），**再**扫一遍挂新的（几十秒）。
///   反过来（先扫后挂）就退化成「等扫完才能搜」→ 承诺作废。
///
/// 【参数】
///   - `load_cached`：读旧索引档 → `None` = 没有档（首次索引）；
///   - `scan`：扫盘 → 条目（**只在**秒开之后才调）；
///   - `attach`：把一批条目挂进引擎 → 返回两个数（见 [`Attached`]）。
pub fn volume_attach_phases<C, S, A>(
    mut load_cached: C,
    mut scan: S,
    mut attach: A,
) -> Result<VolumeAttachOutcome, AttachError>
where
    C: FnMut() -> Option<Vec<search_core::IndexEntry>>,
    S: FnMut() -> Result<Vec<search_core::IndexEntry>, AttachError>,
    A: FnMut(AttachPhase, Vec<search_core::IndexEntry>) -> Result<Attached, AttachError>,
{
    // ① 没有旧档 → 只有一段：扫完挂上（首次索引）。
    let Some(cached) = load_cached() else {
        let fresh = scan()?;
        let a = attach(AttachPhase::Final, fresh)?;
        return Ok(VolumeAttachOutcome::Indexed { own: a.own });
    };
    // ② 命中旧档 → **先秒开**（用户马上能搜到旧内容）。
    let instant = attach(AttachPhase::Instant, cached)?;
    // ③ **再刷新** —— 扫描必须排在秒开**之后**（否则用户要干等扫描）。
    let fresh = scan()?;
    let last = attach(AttachPhase::Final, fresh)?;
    Ok(VolumeAttachOutcome::Refreshed {
        cached_own: instant.own,
        fresh_own: last.own,
    })
}

/// 附加失败之后，隔多久再试一次。
///
/// 【为什么要节流】失败重试会**重新扫一遍那个盘**（几十万文件 → 几十秒）
///   → 每秒重试一次 = 机器被扫描烤着。
///   → 隔 30 秒再试：既不是「永远不试」，也不是「热循环」。
const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// 这个卷现在该不该起一次附加？
///
/// 【为什么要问】三件事都会让「起一次附加」变成坏事：
///   - 正在附加中 → 再起一个 = 重复扫描同一个盘；
///   - 刚失败过 → 立刻再试 = 每秒重扫一遍几十万文件的大盘；
///   - 还没到重试间隔 → 同上。
pub fn should_attach(
    volume: &Path,
    pending: &HashSet<PathBuf>,
    failed_at: &HashMap<PathBuf, Instant>,
    now: Instant,
) -> bool {
    if pending.contains(volume) {
        return false;
    }
    match failed_at.get(volume) {
        Some(t) => now.duration_since(*t) >= RETRY_INTERVAL,
        None => true,
    }
}

/// 处理一批附加结果 → 更新「已挂上」「正在附加」「失败时刻」三张表。
///
/// 【为什么失败不记 `known`】记了就**永远不再试** →
///   那个卷永远进不了索引，直到用户拔插一次（I-4）。
pub fn apply_attach_results(
    known: &mut HashSet<PathBuf>,
    pending: &mut HashSet<PathBuf>,
    failed_at: &mut HashMap<PathBuf, Instant>,
    results: Vec<(PathBuf, AttachResult)>,
    now: Instant,
) {
    for (volume, result) in results {
        // 无论是成是败，它都不再「正在附加」了 → 下一轮可以再起一次。
        pending.remove(&volume);
        match result {
            AttachResult::Ok => {
                known.insert(volume.clone());
                failed_at.remove(&volume);
            }
            AttachResult::Error(AttachError::EngineNotReady) => {
                // 引擎还没装好 → **不记 known** → 下一轮就再试（没扫过盘，不贵）。
                failed_at.remove(&volume);
            }
            AttachResult::Error(AttachError::Vanished) => {
                // 盘被拔了 → 不用再试（它自己会从挂载表里消失）。
                failed_at.remove(&volume);
            }
            AttachResult::Error(AttachError::Failed(e)) => {
                crate::logging::log_line(&format!(
                    "[volumes] {} 附加失败（{e}），稍后重试",
                    volume.display()
                ));
                // 【失败不记 known】—— 记了就永远不再试。
                failed_at.insert(volume, now);
            }
        }
    }
}

/// 对账：`known` 里**不在引擎实际索引的根**里的卷 → 剔除（并返回它们）。
///
/// 【为什么要有这条对账】`known` 是监控线程的**局部变量**，
///   与引擎换代**完全解耦** → 用户换索引目录时若卷枚举失败
///   （新引擎里没有那个卷），`known` 里还留着它 →
///   下一轮差分看到「没变化」→ **永远不会重新附加**（I-2）。
///   → 这是一条**通用不变式**：将来任何让引擎换代的动作都会自动受益。
///
/// 【判据】「引擎实际在索引的根」= `IndexState.paths`
///   （它装的就是引擎实际那一份根，见 `index_state.rs` 的说明）。
pub fn reconcile_known(known: &mut HashSet<PathBuf>, engine_roots: &[String]) -> Vec<PathBuf> {
    let mut dropped: Vec<PathBuf> = Vec::new();
    known.retain(|v| {
        let s = v.to_string_lossy();
        let still = engine_roots.iter().any(|r| r == &*s);
        if !still {
            dropped.push(v.clone());
        }
        still
    });
    dropped.sort();
    dropped
}

/// 当前所有卷的状态 —— **拿不到状态就当没有卷**。
///
/// 【为什么用 `try_state` 而不是 `state`】
///   `state()` 在状态没被 manage 时会 **panic** ——
///   而 mock app（测试）与「状态还没注册」的早期路径上确实没有它。
///   → 拿不到就给空列表（= 状态栏不显示卷那一行，**不是故障**）。
pub fn volume_statuses<R: Runtime>(app: &AppHandle<R>) -> Vec<VolumeStatus> {
    app.try_state::<VolumeState>()
        .map(|s| s.snapshot())
        .unwrap_or_default()
}

/// 记一个卷的状态 —— **状态没注册就跳过**。
///
/// 【为什么不用 `state()`】`state()` 在状态没被 manage 时会 panic ——
///   而这里跑在**监控线程**里：panic 会让整条监控链**静默死掉**
///   （卷从此不再被索引，而且没有任何报错）。
///   → 拿不到就跳过状态更新（功能照常，只是状态栏少那一行）。
fn set_volume_state<R: Runtime>(
    app: &AppHandle<R>,
    path: &Path,
    phase: VolumePhase,
    indexed: u32,
) {
    if let Some(state) = app.try_state::<VolumeState>() {
        state.set(path, phase, indexed);
    }
}

/// 把一个卷从状态里去掉 —— **状态没注册就跳过**（理由同 [`set_volume_state`]）。
fn remove_volume_state<R: Runtime>(app: &AppHandle<R>, path: &Path) {
    if let Some(state) = app.try_state::<VolumeState>() {
        state.remove(path);
    }
}

/// 起监控线程：每秒枚举一次挂载点，驱动附加/分离。
pub fn spawn_volume_monitor<R: Runtime>(app: AppHandle<R>) {
    let spawned = std::thread::Builder::new()
        .name("lodestar-volumes".into())
        .spawn(move || {
            // 【known 装的是什么】上一次已知的卷 —— **挂载点路径本身**。
            //   ⚠️ 它必须是**同一个 PathBuf**：附加与分离用的是「索引空间写法」，
            //   拼写不同 → 摘根静默无效 → **幽灵条目**（盘都拔了还搜得到它的文件）。
            //   → 这里存的与附加时用的那份**逐字相同**（都来自同一轮枚举）。
            let mut known: HashSet<PathBuf> = HashSet::new();
            // 【正在附加中的卷】—— 别重复起线程（会重复扫描同一个盘）。
            let mut pending: HashSet<PathBuf> = HashSet::new();
            // 【上次失败的时刻】—— 决定多久再试一次（见 `RETRY_INTERVAL`）。
            let mut failed_at: HashMap<PathBuf, Instant> = HashMap::new();
            // 【附加结果回传通道】卷线程把「挂上了吗」告诉监控线程。
            //   （扫描要几十秒 → 监控线程**绝不能**等它 → 结果只能异步回传。）
            let (tx, rx): (Sender<(PathBuf, AttachResult)>, Receiver<(PathBuf, AttachResult)>) =
                mpsc::channel();
            loop {
                std::thread::sleep(POLL_INTERVAL);
                // ① 收卷线程的回传 —— 「挂上了吗」是**异步**的。
                let mut results: Vec<(PathBuf, AttachResult)> = Vec::new();
                while let Ok(r) = rx.try_recv() {
                    results.push(r);
                }
                apply_attach_results(&mut known, &mut pending, &mut failed_at, results, Instant::now());
                // 【枚举失败 ≠ 没有卷】失败时**这一轮什么都不做**（别把瞬时错误
                //   当成「所有卷都被拔了」→ 触发不必要的拆索引 + 重新全量扫）。
                // ⚠️ 失败日志 `mounted_local_volumes()` 内部**已经打过一条**了
                //   （带 errno）→ 这里不要再打一遍（否则日志里同一件事两行）。
                let Ok(list) = mounted_local_volumes() else {
                    continue;
                };
                let now: HashSet<PathBuf> = list.into_iter().collect();
                // ② 对账 —— `known` 与「引擎实际在索引的根」对齐。
                //   【为什么不能只看差分】`known` 与引擎换代是**解耦**的：
                //   换索引目录时若枚举失败（新引擎里没有那个卷），
                //   `known` 里还留着它 → 差分看到「没变化」→ 永远不重新附加。
                let engine_roots = app
                    .try_state::<crate::index_state::IndexState>()
                    .map(|s| s.engine_snapshot().1)
                    .unwrap_or_default();
                for v in reconcile_known(&mut known, &engine_roots) {
                    crate::logging::log_line(&format!(
                        "[volumes] {} 已不在引擎的索引里（引擎换代？）→ 剔除并重新附加",
                        v.display()
                    ));
                    // 状态也要清 —— 否则状态栏还亮着一个已经不存在的卷。
                    remove_volume_state(&app, &v);
                    failed_at.remove(&v);
                }
                // ③ 差分 —— 谁来了（要附加）、谁走了（要分离）。
                let (appeared, disappeared) = diff_volumes(&known, &now);
                for v in disappeared {
                    detach_volume(&app, &v);
                    known.remove(&v);
                    pending.remove(&v);
                    failed_at.remove(&v);
                }
                for v in appeared {
                    // 【只有真的接下了才记进 known】——
                    //   附加失败（扫描出错 / 被判重叠）**不记** → 下一轮它还是
                    //   「新出现的」→ 会再试一次（记了就永远不再试 → 那个卷
                    //   永远进不了索引，直到用户拔插一次）。
                    if !should_attach(&v, &pending, &failed_at, Instant::now()) {
                        continue;
                    }
                    if attach_volume(&app, &v, tx.clone()) {
                        pending.insert(v);
                    }
                }
                // 【卷被改名了会怎样】
                //   macOS 改名 = 卸载 + 以新名字挂载 → 这里看到「旧的消失 + 新的出现」
                //   → 旧的那个被分离、新的那个当**新盘**处理（没有档 → 全量扫）。
                //   旧盘留下的档会变成孤儿文件（**暂不清理**，见设计文档 §8）。
            }
        });
    if let Err(e) = spawned {
        eprintln!("[lodestar] 起卷监控线程失败，外接盘不会被索引: {e}");
    }
}

/// 把一个卷从索引里摘掉。
fn detach_volume<R: Runtime>(app: &AppHandle<R>, volume: &Path) {
    let Some(searcher) = current_searcher(app) else {
        // 没有引擎 → 这个卷不可能在索引里 → 状态也不该留着。
        remove_volume_state(app, volume);
        return;
    };
    // 【⚠️ 这里传的必须是「与挂时逐字相同」的那份路径】
    //   `detach_root` 比的是**索引空间写法**，不是规范化后的真实路径 ——
    //   拼写不同 → 静默无效 → 条目留着 = 幽灵条目。
    //   → 调用方传的就是 `known` 里存的那份 PathBuf（与附加时同一个）✓
    match searcher.detach_root(volume) {
        Ok(indexed) => {
            crate::logging::log_line(&format!(
                "[volumes] 已摘掉 {}（剩 {indexed} 项）",
                volume.display()
            ));
            remove_volume_state(app, volume);
            // 路径也要从状态里去掉 —— 否则 watcher 会继续监听已消失的路径
            remove_path(app, volume, indexed);
            // 索引变了 → 前端要重搜
            notify_frontend(app);
        }
        Err(e) => {
            // 【这不是故障】「没挂过」才会走到这里 ——
            //   拔得太快（附加线程还在扫）或那次附加本来就失败了。
            //   → 引擎里没有它的条目 → **没有幽灵条目要清**。
            //   （「挂过但拼写不同」那种真幽灵，worker 侧已经挡住了。）
            crate::logging::log_line(&format!(
                "[volumes] 摘掉 {} 时它没挂过（忽略）: {e}",
                volume.display()
            ));
            // 【但状态要去掉】——
            //   任何「附加没成功」的卷（扫描出错、被判重叠、附加时盘已被拔掉）
            //   在状态栏上都留下一行（「U盘 · 索引失败」）→
            //   拔掉之后那一行**永远留着** → 用户看到的是一个已经不存在的盘。
            remove_volume_state(app, volume);
            // 前端那一行也要清掉 → 必须发一次通知。
            notify_frontend(app);
        }
    }
}

/// 把一个卷加进索引。**返回 `true` = 线程起动了**（**不是**「挂上了」）。
///
/// 【为什么结果是异步的】扫描一个几十万文件的盘要几十秒 →
///   监控线程**绝不能**等它（等它就检测不到拔盘了）→
///   → 结果由卷线程经 channel 回传（见 [`AttachResult`]），
///     监控线程收到「挂上了」才把它记进 `known`。
///
/// 【为什么这样才对】以前这个 `true` 只表示「线程起动了」，
///   监控线程就把它记进了 `known` → 附加失败（扫描出错 / 被判重叠）
///   的卷**永远不再试** → 那个卷永远进不了索引，直到用户拔插一次（I-4）。
fn attach_volume<R: Runtime>(
    app: &AppHandle<R>,
    volume: &Path,
    tx: Sender<(PathBuf, AttachResult)>,
) -> bool {
    // 【引擎还没装好就别扫】——
    //   启动路径上引擎要建一会儿（$HOME 量级 1 秒），那段时间里没有引擎可挂。
    //   这时候扫出来的条目**无处可挂** → 白扫一遍（几十万文件 → 几十秒）。
    //   → 返回 false = 「这一轮不接手」→ 监控线程**不把它记进 known**
    //     → 下一秒再试（引擎好了就能挂上）。
    if current_searcher(app).is_none() {
        return false;
    }
    let app = app.clone();
    // 【⚠️ 路径必须是【逐字相同】的那一份】——
    //   附加与分离都要用「同一个 PathBuf」→ 这里 clone 出来的这份
    //   与监控线程 `known` 里存的那份逐字相同 ✓（两边都来自同一轮枚举）。
    let volume = volume.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("lodestar-volume-attach".into())
        .spawn(move || {
            set_volume_state(&app, &volume, VolumePhase::Indexing, 0);
            // ── 两段：秒开 + 后台刷新 ──
            //   【为什么拆两段】命中旧索引档 → 立刻挂上去（毫秒级，用户马上能搜），
            //   但档是**拔插期间**的快照 → 拔插期间在别的机器上拷进盘里的文件
            //   **不在档里**（FSEvents 也不会补发历史事件）→
            //   → 必须**再扫一遍**挂上去，否则那些文件**永远**进不了索引（且不自愈）。
            let outcome = volume_attach_phases(
                || load_cached_entries(&app, &volume),
                || scan_volume(&volume),
                |phase, entries| {
                    // 【拔盘竞态】扫的这几十秒里用户可能把盘拔了 →
                    //   复查一次，没挂载就丢弃结果（不把死盘的条目塞进索引）。
                    // 【枚举失败时保守处理】拿不到清单就当「还在」（不丢弃结果）
                    let still_there = mounted_local_volumes()
                        .map(|list| list.contains(&volume))
                        .unwrap_or(true);
                    if !still_there {
                        return Err(AttachError::Vanished);
                    }
                    // 【引擎要在扫完【之后】才取】——
                    //   扫的这几十秒里用户可能换了索引目录（重建引擎）→
                    //   扫之前取到的引擎那时已经过期了 → 挂到旧引擎上 → 白扫。
                    let Some(searcher) = current_searcher(&app) else {
                        return Err(AttachError::EngineNotReady);
                    };
                    // 【这个卷自己有几条】—— 必须在 entries 被 move 之前取！
                    //   `attach_root` 返回的是**整个引擎**的条目总数（含 $HOME），
                    //   拿它去填状态栏 → 「U盘 · 8,150 项」（$HOME 8,000 + U 盘 150）。
                    let own = entries.len() as u32;
                    let total = searcher
                        .attach_root(&volume, entries)
                        .map_err(|e| AttachError::Failed(e.to_string()))?;
                    commit_attached(&app, &volume, phase, Attached { own, total });
                    // 索引变了 → 前端要重搜（秒开那一次也要发 → 用户马上能搜到）。
                    notify_frontend(&app);
                    Ok(Attached { own, total })
                },
            );
            let result = match outcome {
                Ok(VolumeAttachOutcome::Indexed { own }) => {
                    crate::logging::log_line(&format!(
                        "[volumes] {} 首次索引完成（{own} 项）",
                        volume.display()
                    ));
                    AttachResult::Ok
                }
                Ok(VolumeAttachOutcome::Refreshed {
                    cached_own,
                    fresh_own,
                }) => {
                    crate::logging::log_line(&format!(
                        "[volumes] {} 后台刷新完成（{cached_own} → {fresh_own} 项）",
                        volume.display()
                    ));
                    AttachResult::Ok
                }
                Err(AttachError::Vanished) => {
                    crate::logging::log_line(&format!(
                        "[volumes] {} 扫描期间被拔掉，丢弃",
                        volume.display()
                    ));
                    // 【秒开那一份要一起摘掉】——
                    //   「秒开」阶段**已经**挂上去一份（档里的条目）→ 盘被拔了 →
                    //   那些条目必须一起消失 → 否则用户还能搜到**已经拔掉的**盘上的文件
                    //   （用户最不能接受的那种幽灵）。
                    //   → 这里摘一次（引擎里本来没挂过也不怕：
                    //     `detach_root` 只摘挂过的根，见引擎层那条不变式）。
                    detach_volume(&app, &volume);
                    AttachResult::Error(AttachError::Vanished)
                }
                Err(AttachError::EngineNotReady) => {
                    crate::logging::log_line(&format!(
                        "[volumes] {} 附加时引擎还没装好，等下一轮",
                        volume.display()
                    ));
                    AttachResult::Error(AttachError::EngineNotReady)
                }
                Err(AttachError::Failed(e)) => {
                    set_volume_state(&app, &volume, VolumePhase::Failed(e.clone()), 0);
                    AttachResult::Error(AttachError::Failed(e))
                }
            };
            // 【把结果回传给监控线程】—— 「挂上了吗」只能异步告诉它。
            let _ = tx.send((volume.clone(), result));
        });
    if let Err(e) = spawned {
        eprintln!("[lodestar] 起卷附加线程失败: {e}");
        return false;
    }
    true
}

/// 拿当前活着的引擎句柄。
fn current_searcher<R: Runtime>(app: &AppHandle<R>) -> Option<Arc<search_core::Searcher>> {
    let state = app.try_state::<Mutex<Option<Arc<search_core::Searcher>>>>()?;
    let guard = state.inner().lock().ok()?;
    guard.clone()
}

/// 读这个卷的**旧索引档** —— 命中就是「秒开」的那一份。
///
/// 【为什么要读档】插回来的盘**上次扫过** → 档还在 → 读档就是毫秒级，
///   用户立刻能搜到。
///
/// ⚠️ **但这只是一半** —— 档是**拔插期间**的快照：
///   拔插期间在别的机器上往盘里拷的文件**不在档里**
///   （FSEvents 不会补发历史事件）→ 读档之后**还要再扫一遍**
///   （见 [`volume_attach_phases`]）。第一版就是在这里**早退**了，
///   承诺的那一半（后台刷新）**根本没实现**，而且是静默的。
fn load_cached_entries<R: Runtime>(
    app: &AppHandle<R>,
    volume: &Path,
) -> Option<Vec<search_core::IndexEntry>> {
    // 【索引档目录的唯一真相在 `settings::index_store_dir`】——
    //   写档的地方（建引擎）与读档的地方（这里）必须是**同一个目录**，
    //   否则卷永远命不中旧档 → 每次插上都要全量重扫。
    let dir = crate::settings::index_store_dir(app)?;
    let entries = search_core::store::load_root(&dir, volume)?;
    crate::logging::log_line(&format!(
        "[volumes] {} 命中旧索引档，秒开",
        volume.display()
    ));
    Some(entries)
}

/// 扫一个卷的全部条目（**真的去读盘**）。
///
/// 【为什么这里只管扫】「读档 → 秒开 → 再扫 → 挂新的」那个编排
///   在 [`volume_attach_phases`] 里 —— 这里只负责「把盘读一遍」。
fn scan_volume(volume: &Path) -> Result<Vec<search_core::IndexEntry>, AttachError> {
    search_core::scan::scan(&[volume.to_path_buf()]).map_err(|e| AttachError::Failed(e.to_string()))
}

/// 挂上之后提交状态 —— **卷自己的条数**进卷状态，**引擎总数**进索引状态。
///
/// 【为什么必须分开】`attach_root` 返回的是**整个引擎**的条目总数。
///   拿它去填状态栏 → 「U盘 · 8,150 项」（$HOME 8,000 + U 盘 150）→
///   **这个数字是错的**，而它是用户唯一能看到的「盘上到底有多少东西」的指标。
///   （同一个数在两处被当成两种语义用过 —— 典型的跨任务语义漂移。）
///
/// 【秒开之后为什么还是 Indexing】后台刷新还没完 →
///   否则「插回来立刻能搜」与「内容是最新的」这两件事在界面上分不开
///   （以前是「秒开 → 直接 Ready」，用户完全看不出后台正在刷新）。
fn commit_attached<R: Runtime>(
    app: &AppHandle<R>,
    volume: &Path,
    phase: AttachPhase,
    counts: Attached,
) {
    let volume_phase = match phase {
        AttachPhase::Instant => VolumePhase::Indexing,
        AttachPhase::Final => VolumePhase::Ready,
    };
    set_volume_state(app, volume, volume_phase, counts.own);
    commit_paths(app, volume, counts.total);
}

/// 把「引擎索引了哪些目录」更新到状态里（卷加进来了）。
///
/// 【为什么必须更新】
///   `watcher::desired_watch_dirs` 读的就是 `IndexState.paths` ——
///   不更新的话，新挂上的卷**不会被 FSEvents 监听** → 它的变化永远进不了索引。
fn commit_paths<R: Runtime>(app: &AppHandle<R>, volume: &Path, indexed: u32) {
    let state = app.state::<crate::index_state::IndexState>();
    let (gen, mut paths) = state.engine_snapshot();
    let volume = volume.to_string_lossy().into_owned();
    if !paths.iter().any(|p| p == &volume) {
        paths.push(volume);
    }
    // 【条件提交】期间要是换过引擎 → 这次结果描述的是旧引擎 → 丢弃
    state.commit_if_current(gen, paths, indexed);
}

/// 把一个目录从「引擎索引了哪些目录」里去掉（卷拔掉了）。
///
/// 【为什么必须去掉】
///   `watcher::desired_watch_dirs` 读的就是这份列表 ——
///   不去掉的话 watcher 会**继续监听一个已经不存在的路径**，
///   每秒对账一次都会失败一次（还会在日志里刷屏）。
///
/// 【`indexed` 用的是引擎报回来的总数】——
///   `detach_root` 返回「分离之后的索引条目总数」→ 那是**当前引擎的真实条数**；
///   而 `state.snapshot().indexed` 是上一次提交时的旧值（可能已经过时）。
fn remove_path<R: Runtime>(app: &AppHandle<R>, volume: &Path, indexed: u32) {
    let state = app.state::<crate::index_state::IndexState>();
    let (gen, mut paths) = state.engine_snapshot();
    let volume = volume.to_string_lossy().into_owned();
    paths.retain(|p| p != &volume);
    state.commit_if_current(gen, paths, indexed);
}

/// 通知前端「索引变了，重搜一次」。
fn notify_frontend<R: Runtime>(app: &AppHandle<R>) {
    use tauri::Emitter;
    let mut st = app.state::<crate::index_state::IndexState>().snapshot();
    st.visible = crate::lifecycle::window_visible(app);
    st.volumes = volume_statuses(app);
    if let Err(e) = app.emit(VOLUMES_CHANGED_EVENT, st) {
        eprintln!("[lodestar] 通知前端失败: {e}");
    }
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

    // ── 差分：新出现的 / 消失的（Task 5） ──

    #[test]
    fn newly_mounted_volumes_are_detected() {
        let known: HashSet<PathBuf> = HashSet::new();
        let now: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let (appeared, disappeared) = diff_volumes(&known, &now);
        assert_eq!(appeared, vec![PathBuf::from("/Volumes/U盘")]);
        assert!(disappeared.is_empty());
    }

    #[test]
    fn unplugged_volumes_are_detected() {
        let known: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let now: HashSet<PathBuf> = HashSet::new();
        let (appeared, disappeared) = diff_volumes(&known, &now);
        assert!(appeared.is_empty());
        assert_eq!(disappeared, vec![PathBuf::from("/Volumes/U盘")]);
    }

    #[test]
    fn an_unchanged_set_produces_nothing() {
        let s: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let (appeared, disappeared) = diff_volumes(&s, &s);
        assert!(appeared.is_empty() && disappeared.is_empty());
    }

    // ── 卷状态（状态机本身） ──

    #[test]
    fn a_volume_state_starts_empty() {
        let state = VolumeState::default();
        assert!(state.snapshot().is_empty());
    }

    #[test]
    fn setting_a_volume_records_its_label() {
        let state = VolumeState::default();
        state.set(
            Path::new("/Volumes/U盘"),
            VolumePhase::Indexing,
            0,
        );
        let snap = state.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].path, "/Volumes/U盘");
        assert_eq!(snap[0].label, "U盘", "卷名 = 挂载点的最后一段");
        assert!(matches!(snap[0].phase, VolumePhase::Indexing));
    }

    #[test]
    fn setting_the_same_volume_twice_updates_it_in_place() {
        let state = VolumeState::default();
        state.set(Path::new("/Volumes/U盘"), VolumePhase::Indexing, 0);
        state.set(Path::new("/Volumes/U盘"), VolumePhase::Ready, 42);
        let snap = state.snapshot();
        assert_eq!(snap.len(), 1, "同一个卷不该出现两次");
        assert!(matches!(snap[0].phase, VolumePhase::Ready));
        assert_eq!(snap[0].indexed, 42);
    }

    #[test]
    fn removing_a_volume_takes_it_out_of_the_snapshot() {
        let state = VolumeState::default();
        state.set(Path::new("/Volumes/U盘"), VolumePhase::Ready, 42);
        state.remove(Path::new("/Volumes/U盘"));
        assert!(state.snapshot().is_empty());
    }

    // ── 附加/分离后要更新 `IndexState.paths` ──
    //
    // 【为什么这条很要紧】`watcher::desired_watch_dirs` 读的就是那份列表 ——
    //   不更新的话，新挂上的卷**不会被 FSEvents 监听** → 它的变化永远进不了索引。

    /// 一个带 `IndexState` 的 mock app —— 够跑 `commit_paths` / `remove_path` 了。
    fn mock_app_with_index_state() -> tauri::App<tauri::test::MockRuntime> {
        let app = tauri::test::mock_app();
        app.manage(crate::index_state::IndexState::new());
        app
    }

    #[test]
    fn attaching_a_volume_adds_it_to_the_watched_paths() {
        let app = mock_app_with_index_state();
        let handle = app.handle().clone();
        commit_paths(&handle, Path::new("/Volumes/U盘"), 123);
        let state = handle.state::<crate::index_state::IndexState>();
        let (_, paths) = state.engine_snapshot();
        assert!(
            paths.iter().any(|p| p == "/Volumes/U盘"),
            "挂上的卷必须进 `IndexState.paths`（watcher 靠它监听）：{paths:?}"
        );
        assert_eq!(state.snapshot().indexed, 123, "条目数也要一起提交");
    }

    #[test]
    fn attaching_the_same_volume_twice_does_not_duplicate_the_path() {
        let app = mock_app_with_index_state();
        let handle = app.handle().clone();
        commit_paths(&handle, Path::new("/Volumes/U盘"), 1);
        commit_paths(&handle, Path::new("/Volumes/U盘"), 2);
        let (_, paths) = handle
            .state::<crate::index_state::IndexState>()
            .engine_snapshot();
        assert_eq!(
            paths.iter().filter(|p| *p == "/Volumes/U盘").count(),
            1,
            "同一个卷不该被列两次：{paths:?}"
        );
    }

    #[test]
    fn detaching_a_volume_removes_it_from_the_watched_paths() {
        let app = mock_app_with_index_state();
        let handle = app.handle().clone();
        commit_paths(&handle, Path::new("/Volumes/U盘"), 123);
        remove_path(&handle, Path::new("/Volumes/U盘"), 7);
        let state = handle.state::<crate::index_state::IndexState>();
        let (_, paths) = state.engine_snapshot();
        assert!(
            !paths.iter().any(|p| p == "/Volumes/U盘"),
            "拔掉的卷必须从 `IndexState.paths` 里去掉（否则 watcher 继续监听死路径）：{paths:?}"
        );
        assert_eq!(state.snapshot().indexed, 7, "条目数也要一起提交");
    }

    // ── C-1：「插回来秒开 + 后台刷新」两段（护栏） ──
    //
    // 【为什么这条必须有】
    //   第一版实现把「命中旧索引档」写成了**早退**（`scan_volume` 直接 `return`）
    //   → 「插回来后台刷新」这个承诺**根本没实现**，而且是**静默**的：
    //   用户插上刚在别的机器上拷过文件的 U 盘 → 搜不到那个文件 →
    //   没有报错、状态栏还显示「U盘 · N 项 · 就绪」，而且**不自愈**
    //   （FSEvents 不会补发拔插期间的历史事件）。
    //   → 这里用一条测试钉住两段都在，而且**顺序**不能反。

    /// 造一条最小的索引条目（测试用）。
    fn test_entry(name: &str) -> search_core::IndexEntry {
        let path = format!("/Volumes/U盘/{name}");
        search_core::IndexEntry {
            name: name.to_string(),
            path: path.clone(),
            dir: "/Volumes/U盘".to_string(),
            extension: String::new(),
            size: 1,
            mtime: 1_700_000_000,
            is_folder: false,
            depth: 1,
            child_count: 0,
            child_file_count: 0,
            child_folder_count: 0,
            folded_name: name.to_lowercase(),
            folded_path: path.to_lowercase(),
        }
    }

    #[test]
    fn a_reinserted_volume_is_refreshed_after_the_instant_open() {
        // 命中旧档 → **两段**：先秒开（Instant），再扫描、再挂新的（Final）。
        let log: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let cached = vec![test_entry("old.txt")];
        let fresh = vec![test_entry("new.txt")];
        let out = volume_attach_phases(
            || {
                log.borrow_mut().push("读档".to_string());
                Some(cached.clone())
            },
            || {
                log.borrow_mut().push("扫描".to_string());
                Ok(fresh.clone())
            },
            |phase, entries| {
                log.borrow_mut()
                    .push(format!("挂载({phase:?}) {}", entries[0].name));
                Ok(Attached {
                    own: entries.len() as u32,
                    total: 8_000,
                })
            },
        )
        .expect("附加不该失败");
        assert_eq!(
            *log.borrow(),
            vec![
                "读档".to_string(),
                "挂载(Instant) old.txt".to_string(),
                "扫描".to_string(),
                "挂载(Final) new.txt".to_string(),
            ],
            "命中旧档必须是「读档 → 秒开 → 扫描 → 挂新的」四步 —— \
             少了「扫描 + 挂新的」这两步，拔插期间拷进盘里的文件就**永远**进不了索引"
        );
        assert!(
            matches!(out, VolumeAttachOutcome::Refreshed { .. }),
            "两段都跑完才算「刷新完成」"
        );
    }

    #[test]
    fn a_volume_without_an_archive_is_indexed_only_once() {
        // 没有旧档 → 只有一次（首次索引）：读档 → 扫描 → 挂。
        let log: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let out = volume_attach_phases(
            || {
                log.borrow_mut().push("读档".to_string());
                None
            },
            || {
                log.borrow_mut().push("扫描".to_string());
                Ok(vec![test_entry("a.txt")])
            },
            |phase, entries| {
                log.borrow_mut().push(format!("挂载({phase:?})"));
                Ok(Attached {
                    own: entries.len() as u32,
                    total: 1,
                })
            },
        )
        .expect("附加不该失败");
        assert_eq!(
            *log.borrow(),
            vec!["读档".to_string(), "扫描".to_string(), "挂载(Final)".to_string()],
            "没有旧档时不该有多余的秒开"
        );
        assert!(matches!(out, VolumeAttachOutcome::Indexed { .. }));
    }

    #[test]
    fn a_failed_scan_does_not_attach_anything() {
        // 扫描失败 → 一条都不许挂（挂半份条目 = 索引里有一堆指向空处的幽灵）。
        let attached = std::cell::Cell::new(0usize);
        let res = volume_attach_phases(
            || None,
            || Err(AttachError::Failed("读不出来".to_string())),
            |_, entries| {
                attached.set(attached.get() + entries.len());
                Ok(Attached {
                    own: entries.len() as u32,
                    total: 0,
                })
            },
        );
        assert!(res.is_err(), "扫描失败必须报出来");
        assert_eq!(attached.get(), 0, "扫描失败时一条都不许挂");
    }

    // ── I-1：状态栏上的条数是**这个卷自己的** ──

    #[test]
    fn the_volume_state_shows_the_volumes_own_count() {
        // 状态栏显示的是**这个卷自己**的条目数；
        // `IndexState.indexed` 才是**整个引擎**的总数。
        // 以前两处用的是同一个数（`attach_root` 返回的全局总数）
        // → $HOME 8,000 项 + U 盘 150 个文件 → 状态栏显示「U盘 · 8,150 项」。
        let app = mock_app_with_index_state();
        app.manage(VolumeState::default());
        let handle = app.handle().clone();
        commit_attached(
            &handle,
            Path::new("/Volumes/U盘"),
            AttachPhase::Final,
            Attached {
                own: 150,
                total: 8_150,
            },
        );
        let vols = volume_statuses(&handle);
        assert_eq!(vols.len(), 1);
        assert_eq!(
            vols[0].indexed, 150,
            "状态栏显示的必须是**这个卷自己**的条目数（不是引擎总数）"
        );
        assert_eq!(
            handle
                .state::<crate::index_state::IndexState>()
                .snapshot()
                .indexed,
            8_150,
            "索引状态里的总数是**整个引擎**的条目数"
        );
    }

    // ── I-2：引擎换代后，卷不能被永久丢掉 ──

    #[test]
    fn a_volume_that_is_no_longer_in_the_engine_is_dropped_from_known() {
        // 换索引目录时枚举失败 → 新引擎里没有那个卷 →
        // 监控线程的 `known` 里还留着它 → 差分看到「没变化」→ 永远不重新附加。
        let mut known: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let stale = reconcile_known(&mut known, &["/Users/me".to_string()]);
        assert_eq!(stale, vec![PathBuf::from("/Volumes/U盘")]);
        assert!(
            known.is_empty(),
            "剔除之后它要重新变成「新出现的」→ 下一轮重新附加"
        );
    }

    #[test]
    fn a_volume_that_is_still_in_the_engine_stays_known() {
        let mut known: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let stale = reconcile_known(
            &mut known,
            &["/Users/me".to_string(), "/Volumes/U盘".to_string()],
        );
        assert!(stale.is_empty(), "还在引擎里的卷不该被剔除");
        assert_eq!(known.len(), 1);
    }

    // ── I-3：拔盘后状态栏那一行必须消失 ──

    #[test]
    fn detaching_a_volume_that_was_never_attached_still_clears_its_state() {
        // 「附加没成功」的卷（扫描出错 / 被判重叠 / 盘已被拔）
        // 在状态栏上留下一行「U盘 · 索引失败」→ 用户看到的是一个已经不存在的盘。
        let dir = std::env::temp_dir().join(format!(
            "lodestar-vol-detach-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (engine, _) = search_core::Engine::new(&[&dir]).unwrap();
        let app = mock_app_with_index_state();
        app.manage(VolumeState::default());
        app.manage(Mutex::new(Some(Arc::new(search_core::Searcher::new(engine)))));
        let handle = app.handle().clone();
        // 先给它留一行状态（模拟「附加失败但状态留下了」）
        set_volume_state(
            &handle,
            Path::new("/Volumes/U盘"),
            VolumePhase::Failed("坏了".to_string()),
            0,
        );
        assert_eq!(volume_statuses(&handle).len(), 1);

        // 摘一个**没挂过**的根 → 引擎回 Err → 状态也必须清掉
        detach_volume(&handle, Path::new("/Volumes/U盘"));
        assert!(
            volume_statuses(&handle).is_empty(),
            "摘掉之后状态栏上不能还留着那一行（用户看到的是一个已经不存在的盘）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── I-4：附加失败要能重试 ──

    #[test]
    fn a_failed_attach_is_not_remembered_as_attached() {
        let mut known: HashSet<PathBuf> = HashSet::new();
        let mut pending: HashSet<PathBuf> = HashSet::new();
        let mut failed_at: HashMap<PathBuf, Instant> = HashMap::new();
        apply_attach_results(
            &mut known,
            &mut pending,
            &mut failed_at,
            vec![(
                PathBuf::from("/Volumes/U盘"),
                AttachResult::Error(AttachError::Failed("读不出来".to_string())),
            )],
            Instant::now(),
        );
        assert!(
            known.is_empty(),
            "附加失败**不能**记进 known —— 记了就永远不再试（那个卷永远进不了索引）"
        );
        assert!(
            pending.is_empty(),
            "失败之后要从「正在附加」里拿掉，否则不会再起一次"
        );
    }

    #[test]
    fn a_successful_attach_is_remembered_as_attached() {
        let mut known: HashSet<PathBuf> = HashSet::new();
        let mut pending: HashSet<PathBuf> = [PathBuf::from("/Volumes/U盘")].into_iter().collect();
        let mut failed_at: HashMap<PathBuf, Instant> = HashMap::new();
        apply_attach_results(
            &mut known,
            &mut pending,
            &mut failed_at,
            vec![(PathBuf::from("/Volumes/U盘"), AttachResult::Ok)],
            Instant::now(),
        );
        assert!(known.contains(&PathBuf::from("/Volumes/U盘")));
        assert!(pending.is_empty());
    }

    #[test]
    fn a_failed_attach_is_retried_but_not_in_a_hot_loop() {
        // 失败之后要能重试（不重试 = 那个卷永远进不了索引），
        // 但也不能**每秒**重扫一遍几十万文件的大盘 → 要有节流。
        let volume = PathBuf::from("/Volumes/U盘");
        let now = Instant::now();
        let mut failed_at: HashMap<PathBuf, Instant> = HashMap::new();
        failed_at.insert(volume.clone(), now);
        assert!(
            !should_attach(&volume, &HashSet::new(), &failed_at, now),
            "刚失败就该等一会儿（否则每秒重扫一遍大盘）"
        );
        assert!(
            should_attach(
                &volume,
                &HashSet::new(),
                &failed_at,
                now + RETRY_INTERVAL + Duration::from_millis(1)
            ),
            "过了重试间隔就要再试一次（不重试 = 那个卷永远进不了索引）"
        );
    }

    #[test]
    fn a_volume_that_is_being_attached_is_not_attached_twice() {
        let volume = PathBuf::from("/Volumes/U盘");
        let pending: HashSet<PathBuf> = [volume.clone()].into_iter().collect();
        assert!(
            !should_attach(&volume, &pending, &HashMap::new(), Instant::now()),
            "正在附加的卷不能再起一个线程（会重复扫描）"
        );
    }

    /// 摘掉一个卷 → 它在引擎里的条目必须**一起消失**（不能留幽灵）。
    ///
    /// 【为什么这条很要紧】「秒开」阶段会先挂上去一份（档里的条目）→
    ///   盘在后台刷新期间被拔掉 → 那份条目必须一起摘掉 →
    ///   否则用户还能搜到**已经拔掉的**盘上的文件（用户最不能接受的那种幽灵）。
    #[test]
    fn detaching_a_volume_removes_its_entries_from_the_engine() {
        let dir = std::env::temp_dir().join(format!(
            "lodestar-vol-ghost-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("只有这个盘有.txt"), "x").unwrap();
        let (engine, _) = search_core::Engine::new(&[&dir]).unwrap();
        let searcher = Arc::new(search_core::Searcher::new(engine));
        let app = mock_app_with_index_state();
        app.manage(VolumeState::default());
        app.manage(Mutex::new(Some(searcher.clone())));
        let handle = app.handle().clone();

        let req = || search_core::SearchRequest {
            query: "只有这个盘有".to_string(),
            sort: search_core::SortField::Name,
            order: search_core::SortOrder::Asc,
            kind: search_core::ListKind::All,
            page: 0,
            page_size: 10,
        };
        assert_eq!(
            searcher.search(req()).unwrap().total_entries,
            1,
            "先确认它进索引了"
        );

        detach_volume(&handle, &dir);

        assert_eq!(
            searcher.search(req()).unwrap().total_entries,
            0,
            "摘掉之后它的条目必须消失 —— 留着就是**幽灵条目**（盘都拔了还搜得到）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── M-5：命中排除规则名字的卷整个跳过 ──
    #[test]
    fn a_volume_whose_name_hits_the_exclude_rules_is_skipped() {
        // 一个叫 `Library` 的外接卷 → 扫描会把它整棵树剪掉 → 0 条目
        // → 状态栏显示「Library · 就绪 · 0 项」，用户不知道为什么。
        let mounts = vec![(PathBuf::from("/Volumes/Library"), MNT_LOCAL)];
        assert!(
            filter_local_volumes(&mounts).is_empty(),
            "命中排除规则名字的卷不该被索引（它会被扫描整棵剪掉）"
        );
    }

    #[test]
    fn a_hidden_volume_is_skipped() {
        let mounts = vec![(PathBuf::from("/Volumes/.隐藏盘"), MNT_LOCAL)];
        assert!(
            filter_local_volumes(&mounts).is_empty(),
            "隐藏名的卷同样会被整棵剪掉"
        );
    }

    #[test]
    fn a_normal_volume_is_still_picked_up() {
        let mounts = vec![(PathBuf::from("/Volumes/U盘"), MNT_LOCAL)];
        assert_eq!(
            filter_local_volumes(&mounts),
            vec![PathBuf::from("/Volumes/U盘")]
        );
    }
}
