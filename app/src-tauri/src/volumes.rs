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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
    /// 已经索引进去多少条。
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
            loop {
                std::thread::sleep(POLL_INTERVAL);
                // 【枚举失败 ≠ 没有卷】失败时**这一轮什么都不做**（别把瞬时错误
                //   当成「所有卷都被拔了」→ 触发不必要的拆索引 + 重新全量扫）。
                // ⚠️ 失败日志 `mounted_local_volumes()` 内部**已经打过一条**了
                //   （带 errno）→ 这里不要再打一遍（否则日志里同一件事两行）。
                let Ok(list) = mounted_local_volumes() else {
                    continue;
                };
                let now: HashSet<PathBuf> = list.into_iter().collect();
                let (appeared, disappeared) = diff_volumes(&known, &now);
                for v in disappeared {
                    detach_volume(&app, &v);
                    known.remove(&v);
                }
                for v in appeared {
                    // 【只有真的接下了才记进 known】
                    //   接不下 = 引擎还没装好（启动路径上引擎要建一会儿）。
                    //   → 不记 → 下一秒它还是「新出现的」→ 会再试一次。
                    //   （记了就永远不再试 → 那个卷永远进不了索引。）
                    if attach_volume(&app, &v) {
                        known.insert(v);
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
        }
    }
}

/// 把一个卷加进索引。返回 `true` = 这次真的接下了（调用方可以记进 `known`）。
///
/// 【为什么要开一个自己的线程】
///   第一次插上要**全量扫**这个卷（可能几十万文件 → 几十秒）。
///   那个扫描绝不能占着监控线程（否则拔盘都检测不到），
///   更不能占着搜索 worker（否则搜索全堵死）。
fn attach_volume<R: Runtime>(app: &AppHandle<R>, volume: &Path) -> bool {
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
            match scan_volume(&app, &volume) {
                Ok(entries) => {
                    // 【引擎要在扫完【之后】才取】——
                    //   扫的这几十秒里用户可能换了索引目录（重建引擎）→
                    //   扫之前取到的引擎那时已经过期了 → 挂到旧引擎上 → 白扫。
                    let Some(searcher) = current_searcher(&app) else {
                        return;
                    };
                    // 【拔盘竞态】扫的这几十秒里用户可能把盘拔了 →
                    //   复查一次，没挂载就丢弃结果（不把死盘的条目塞进索引）。
                    // 【枚举失败时保守处理】拿不到清单就当「还在」（不丢弃结果）
                    let still_there = mounted_local_volumes()
                        .map(|list| list.contains(&volume))
                        .unwrap_or(true);
                    if !still_there {
                        crate::logging::log_line(&format!(
                            "[volumes] {} 扫描期间被拔掉，丢弃",
                            volume.display()
                        ));
                        remove_volume_state(&app, &volume);
                        return;
                    }
                    match searcher.attach_root(&volume, entries) {
                        Ok(indexed) => {
                            crate::logging::log_line(&format!(
                                "[volumes] 已挂上 {}（{indexed} 项）",
                                volume.display()
                            ));
                            set_volume_state(&app, &volume, VolumePhase::Ready, indexed);
                            commit_paths(&app, &volume, indexed);
                            notify_frontend(&app);
                        }
                        Err(e) => {
                            set_volume_state(&app, &volume, VolumePhase::Failed(e.to_string()), 0);
                        }
                    }
                }
                Err(e) => {
                    set_volume_state(&app, &volume, VolumePhase::Failed(e.to_string()), 0);
                }
            }
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

/// 扫一个卷的全部条目。
///
/// 【为什么要先读档】插回来的盘**上次扫过** → 档还在 → 读档就是毫秒级，
/// 用户立刻能搜到；卷的新增/删除由 watcher 跟上（见 `watcher.rs`）。
fn scan_volume<R: Runtime>(
    app: &AppHandle<R>,
    volume: &Path,
) -> Result<Vec<search_core::IndexEntry>, String> {
    // 【索引档目录的唯一真相在 `settings::index_store_dir`】——
    //   写档的地方（建引擎）与读档的地方（这里）必须是**同一个目录**，
    //   否则卷永远命不中旧档 → 每次插上都要全量重扫。
    if let Some(dir) = crate::settings::index_store_dir(app) {
        if let Some(entries) = search_core::store::load_root(&dir, volume) {
            crate::logging::log_line(&format!(
                "[volumes] {} 命中旧索引档，秒开",
                volume.display()
            ));
            return Ok(entries);
        }
    }
    search_core::scan::scan(&[volume.to_path_buf()]).map_err(|e| e.to_string())
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
}
