use std::sync::Mutex;
use std::time::Instant;

/// 一次扫描的【标识】——「这是第几趟」。
///
/// 【它现在是什么】`begin()` 返回它。它只用来回答**一个问题**：
///   「这一趟是不是**最新**的那一趟？」—— `fail()` 靠它决定要不要记错。
///
/// 【它不再是什么】它**不再代表「scanning 的所有权」**。
///   以前 `scanning` 是 bool，语义是「**最新那一趟**扫描结束了没有」→
///   只有持最新令牌的那一方能清它 → 别的扫描碰不到。
///   那个模型在**重叠扫描**下是错的：
///     - 新的那趟**先结束** → 标志**提前落地**（旧的那趟还在跑）
///       → 状态栏提前显示「不在扫描」；
///     - 前端 `App.vue` 的「wasScanning && !scanning → 重搜一次」**提前成立**
///       → 搜的是**半成品索引** → 结果表少一批条目，而且**不会自动再刷一次**
///       （标志已经落地了，不会再翻转）。
///   现在 `scanning` 由**在飞扫描数**现算（见 `Inner::scan_count`）→
///   与**结束顺序无关** → 令牌不再需要「主权」这份职责。
///
/// 【为什么还留着它】删了就没法判断「是不是最新那一趟」——
///   `fail()` 的记错条件就少了一条（见 [`IndexState::fail`]）。
///
/// 【内部字段不 pub】外部只该拿它当标识用，不该读它的值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanToken(u64);

/// 「正在建索引 / 已索引多少 / 索引了哪些目录」的共享状态。
/// 前端的状态栏读它。
///
/// 【为什么要收进一把锁】
///   原来五个字段用了**四种**同步原语（`AtomicBool` + `AtomicU32` +
///   `Mutex<Vec<String>>` + `AtomicU64` + `Mutex<Option<String>>`），
///   `snapshot()` 要分三次加锁读 →
///   可能读到「新代次 + 旧路径」这种**自相矛盾**的组合。
///   现在**一次加锁读全部** → 读到的永远自洽。
///
/// 【不变式】`indexed` 与 `paths` **永远描述当前活着的那个引擎**。
///
///   重建失败时旧引擎原封不动（`Engine::new` 失败 → 旧的 `Arc` 压根没被碰过），
///   那状态就必须还报旧引擎的条目数与旧的那组路径。
///   反过来（报「失败的那组路径 + 0 项」）就会与真实引擎错位 ——
///   状态栏说「已索引 0 项」，而结果表里还是旧结果。
///
///   所以：**paths 与 indexed 都等成功才提交**（见 `commit_new_engine`），
///   `begin` 只动在飞计数，`fail` 才记错误。
///
/// 【不变式 2（引擎代次）】`generation` 标识「当前活着的引擎是第几代」。
///
///   **只有换上新的引擎才 +1** —— rescan（同一个引擎上重扫）不 +1。
///   用途：让「跨越了换引擎的旧操作」认出自己的结果已经过期
///   （见 [`IndexState::commit_if_current`]）。
///
/// 【不变式 3（扫描计数）】`scanning` **不是字段**，是 `scan_count > 0` **现算**出来的。
///
///   `scan_count` = **当前还有几趟扫描在跑**
///   （`begin()` +1，`end_scan` / `fail` −1）。
///   → 语义与**结束顺序无关**：重叠时只有**最后一趟**结束，标志才会落地。
///   （旧模型是 bool「最新那一趟结束没有」→ 新的那趟先结束就会**提前落地**。）
///
///   【配对不变式】每一次 `begin()` 都恰好配一次 `end_scan` / `fail` ——
///   由 `run_scan` / `run_scan_blocking` 保证（那两处是所有扫描的收口）。
///
///   【为什么不是 bool】见 `Inner::scan_count` 的说明。
///
/// 【🔒 锁序禁令 —— 本模块的锁永远在最里层】
///   `IndexState` **绝不能**去碰引擎槽位那把锁（`Mutex<Option<Arc<Searcher>>>`）。
///   全局锁序必须是：**槽位锁 → IndexState 锁**。
///
///   【为什么这是硬要求】反过来的话（先拿 IndexState 锁、再拿槽位锁）
///   就会与「槽位锁 → IndexState 锁」构成**环形等待** → 死锁。
///   当前代码里 `commit_new_engine` 是在**持有槽位锁**时调用的，两处都是：
///     - `settings::rebuild_engine`（换索引目录）
///     - `settings::rebuild_engine_if_absent`（启动路径装引擎）
///   —— 那正是「槽位 → IndexState」这个方向 ✓。
///
///   【以后要加什么都要先想一遍】若给 `IndexState` 加一个「回调引擎槽位」的方法
///   （比如反查当前引擎），就会锁序反转 → 死锁。
///   **先拆成两步（读出数据 → 放锁 → 再拿另一把锁）**，不要在一个锁里套另一个。
pub struct IndexState {
    /// 【全部状态都在这一把锁里】—— 一次加锁读全部 → 读到的永远自洽。
    inner: Mutex<Inner>,
}

/// 全部状态 —— **一把锁**。
struct Inner {
    /// 【在飞的扫描数】—— **当前还有几趟扫描在跑**。
    ///
    /// 【为什么是计数器而不是 bool】
    ///   bool 的语义是「**最新那一趟**扫描结束了没有」——
    ///   重叠时**新的那趟先结束**会让它提前落地（旧的那趟还在跑）→
    ///   状态栏提前显示「不在扫描」，
    ///   前端 `App.vue` 的「wasScanning && !scanning → 重搜一次」**提前成立**
    ///   → 结果表可能重搜**半成品索引** → 少一批条目，且不会自动再刷一次。
    ///   计数器的语义是「**还有几趟在跑**」—— 与结束顺序无关，天然自洽。
    ///
    /// 【唯一真相】`scanning` **不作为字段存在**，一律现算 `scan_count > 0`
    ///   → 不会出现「字段和计数器不同步」。
    ///
    /// 【配对不变式】每一次 `begin()` 都恰好配一次 `end_scan` / `fail`
    ///   —— 由 `run_scan` / `run_scan_blocking` 保证（那两处是所有扫描的收口）。
    ///   **靠这条，而不是靠 `saturating_sub` 兜底**：重复收尾会让计数少 1，
    ///   可能出现「还有扫描在跑但 scanning=false」。
    scan_count: usize,
    /// 当前活着的引擎索引了多少项。
    indexed: u32,
    /// 当前活着的引擎索引了哪些目录。
    paths: Vec<String>,
    /// 【引擎代次】当前活着的引擎是第几代。**只有换上新的引擎才 +1**。
    generation: u64,
    /// 最近一次建索引失败的原因（成功时为空）。
    error: Option<String>,
    /// 【扫描序号】每次 `begin()` 都 +1。令牌靠它判新旧。
    ///
    /// 【职责变了】以前它**同时**是「令牌」和「scanning 的所有权凭证」；
    ///   现在它只是「**这一趟是第几趟**」的唯一性标识 +
    ///   「**是不是最新**」的判据（`fail()` 的记错条件之一）。
    ///   `end_scan()` **不再看它**（谁结束都减计数）。
    ///   **别删**：删了就没法判断「是不是最新那一趟」。
    scan_seq: u64,
    /// 最近一次**成功完成**的扫描的**完成**时刻（`None` = 还没成功扫过）。
    ///
    /// 【为什么要有它】—— watcher 判断「这次文件变化是不是已经被某趟扫描覆盖了」
    ///   要用它（见 `watcher.rs` 的 `decide_auto_rescan`）。
    ///   没有它的话：用户点一次「刷新」（= 一趟全量重扫）之后，
    ///   watcher 还会因为**同一次变化**再扫一遍 → 一次手动刷新变成**两次**全量重扫。
    ///
    /// 【为什么记在这里，而不是 watcher 自己记】
    ///   watcher 的计时器只管「两次**自动**重扫之间的间隔」，
    ///   **管不到**用户点的「刷新」—— 那是另一条路（`commands::do_rescan`）。
    ///   要让两边共用同一个事实，就得记在**两边都能读到**的地方 ——
    ///   `IndexState` 已经有 `Mutex<Inner>`，加个字段即可，
    ///   **不要**另开一把锁（会多一处要维护的锁序）。
    ///
    /// 【只记成功】失败的扫描没更新索引 → 覆盖不了任何变化。
    last_scan_completed_at: Option<Instant>,
}

impl IndexState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                scan_count: 0,
                indexed: 0,
                paths: Vec::new(),
                generation: 0,
                error: None,
                scan_seq: 0,
                last_scan_completed_at: None,
            }),
        }
    }

    /// 开始一次重建：**在飞扫描数 +1**，并把上一次的错误清掉，
    /// 返回**这次扫描的标识**。
    ///
    /// 【为什么这里不碰 paths 与 indexed】
    ///   旧引擎此刻还活着，它索引了多少项、索引了哪些目录，状态就该还报什么。
    ///   提前换成新值 → 重建失败时就与真实引擎错位了。
    ///
    /// 【为什么【要】清 error】
    ///   状态栏的错误提示与「正在扫描索引…」是**两个独立的分支** ——
    ///   不清的话，重建期间会同时显示「正在扫描索引…」和
    ///   「索引失败：<上一次那个错>」—— 那条错已经过时了（用户正准备重试）。
    ///   （成功提交时也会清 —— 成功了就没什么可报的。）
    ///
    /// 【在飞计数 +1】这就是「有扫描在跑」的**全部**含义 ——
    ///   重叠时**每一趟**都算一次 → 只有**最后一趟**结束，标志才落地。
    ///   （旧模型是 bool「最新那一趟结束没有」→ 新的那趟先结束就会**提前落地**。）
    ///
    /// 【配对】调用方**必须**保管好返回的标识：结束时用 `end_scan(token)` /
    ///   `fail(token, …)` 把自己这一趟**收掉**（在飞计数 −1）。
    pub fn begin(&self) -> ScanToken {
        let mut inner = self.inner.lock().unwrap();
        inner.scan_seq += 1;
        inner.scan_count += 1;
        inner.error = None;
        ScanToken(inner.scan_seq)
    }

    /// 结束这一趟扫描（成功失败都算）：**在飞扫描数 −1**，**不动** paths 与 indexed。
    ///
    /// 【返回值语义（改过了！）】**结束后是否已无扫描在跑**
    ///   （即 `scan_count == 0`）。
    ///
    ///   【为什么这么定】返回值只有一个用途 —— 告诉调用方「扫描是不是全都结束了」。
    ///   在计数模型下，「结束」不再是「某一趟的动作」，而是「**没有扫描在跑了**」
    ///   这个**事实** → 返回值就该报告这个事实。
    ///   （旧语义是「我把标志清掉了没有」—— 那是 bool 时代的「主权」概念。）
    ///
    /// 【不看令牌】谁结束都减计数 —— 计数的语义与**结束顺序无关**。
    ///   令牌过期（已经有更新的扫描接管）也一样减 ——
    ///   **这一趟确实结束了**，在飞数就该减。
    ///   （旧模型里「令牌过期 → 什么都不做」正是那个 bug 的另一半：
    ///     旧的那趟结束时没人能减掉它 → 标志永远卡在 true。）
    ///
    /// 【什么时候用】有人开始了一次建索引（`begin`），
    ///   但最后决定**不装**（引擎已经被别人装上了）——
    ///   那就得把在飞数放回去。
    ///
    /// 【为什么必须有】不然在飞数会**永远卡在 >0**：
    ///   - 状态栏永远显示「正在扫描索引…」（`StatusBar.vue` 读 `scanning`）；
    ///   - 前端 `App.vue` 的「wasScanning && !s.scanning → 重搜一次」
    ///     **永不成立** → 索引就绪后不会自动重搜（结果一片空白）。
    ///
    /// 【为什么不动 paths/indexed】它们描述的是**当前活着的那个引擎**——
    ///   引擎没换（是别人的），那 paths/indexed 就还是别人的那份，
    ///   不能因为「我们放弃了」就改它们。
    pub fn end_scan(&self, _token: ScanToken) -> bool {
        let mut inner = self.inner.lock().unwrap();
        // 【在飞数减一】—— 不看令牌：谁结束都减。
        //   `saturating_sub` 只是防止**误调两次**时下溢成巨大值 ——
        //   它**掩盖不了**重复收尾（那会让计数少 1）→ 重复收尾要靠测试钉。
        inner.scan_count = inner.scan_count.saturating_sub(1);
        // 【返回值】结束后**是否已无扫描在跑**。
        inner.scan_count == 0
    }

    /// **一次锁内**读出「引擎代次 + 索引了哪些目录」。
    ///
    /// 【为什么取代「先取代次、后取路径」那条顺序约定】
    ///   以前代次是原子量、路径在另一把锁里 → 要凑成一对必须靠调用方
    ///   记住「先读代次、再读路径」这个**顺序约定**（很隐蔽，迟早有人写反）。
    ///   现在两个字段在同一把锁里 → **一次读完，中间没有窗口**。
    ///
    /// 【为什么取代了原来的 `generation()` / `paths()` 两个单字段 getter】
    ///   那两个 getter 生产代码**零调用点**（只挂着 `#[allow(dead_code)]`），
    ///   而这里一次锁内就能给出「代次 + 路径」——
    ///   只看其中一个字段时写 `engine_snapshot().0` / `.1` 就够了。
    ///   留着两个「只给测试用」的生产 API，只会诱使人绕过「一次锁内读全部」那条不变式。
    ///
    /// 【返回】`(代次, 路径)`。
    pub fn engine_snapshot(&self) -> (u64, Vec<String>) {
        let inner = self.inner.lock().unwrap();
        (inner.generation, inner.paths.clone())
    }

    /// **换引擎**之后的提交：代次 +1，然后把路径与条目数提交上去。
    ///
    /// 【为什么必须有这个，而不是继续用无条件提交】
    ///   装引擎有两种：
    ///     - **换上新的引擎**（reindex / 启动路径）→ 状态要改描述新的那个 → 代次 +1；
    ///     - **同一个引擎上重扫**（rescan）→ 状态描述的还是它 → 代次不变。
    ///   分不开这两者，「旧操作跨越了换引擎」就认不出自己的结果已经过期。
    ///
    /// 【调用方必须保证：代次递增发生在【装引擎之后】】
    ///   反过来（先递增、后装引擎）会开一个「新代次 + 旧引擎」的窗口：
    ///   那期间开始的 rescan 提交时校验代次会**通过** →
    ///   把**旧引擎**的结果写进代表新引擎的状态 → **反向错位**，比原 bug 更糟。
    ///   （装引擎 + 递增代次 + 提交状态，三步在**同一把锁内**做完 ——
    ///     见 `settings::rebuild_engine` / `settings::rebuild_engine_if_absent`。）
    ///
    /// 【不动 scanning】在飞扫描数归**扫描的收尾**管
    ///   （`begin()` 谁调，谁负责 `end_scan` / `fail`）。
    ///   提交状态**不代表**扫描结束 —— 装了引擎之后扫描还要再走完收尾。
    pub fn commit_new_engine(&self, paths: Vec<String>, indexed: u32) {
        let mut inner = self.inner.lock().unwrap();
        inner.generation += 1;
        inner.paths = paths;
        inner.indexed = indexed;
        // 成功了就没什么可报的 → 把上一条错误清掉
        inner.error = None;
        // 【不动在飞计数】—— 见上面的说明
    }

    /// **条件提交**：当前代次 == `gen` 才提交（返回 true），
    /// 否则**什么都不做**（返回 false）。
    ///
    /// 【谁用】rescan —— 它在**开始时**快照了（代次 + 路径），
    ///   在**结束时**才提交。若这期间引擎被换掉了，
    ///   它手上那份结果描述的是**旧引擎**（旧路径 / 旧计数），
    ///   提交下去就与当前引擎错位了（状态栏报旧引擎、结果表是新引擎）。
    ///   → 代次对不上就**丢弃**，状态保持换引擎那一方提交的那份（正确的）。
    ///
    /// 【为什么检查与提交要在同一把锁内】
    ///   否则「检查通过」与「真的提交」之间可能又换了一次引擎 → 又错位。
    ///
    /// 【不动 scanning】理由同 [`IndexState::commit_new_engine`]。
    pub fn commit_if_current(&self, gen: u64, paths: Vec<String>, indexed: u32) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.generation != gen {
            // 期间换了引擎 → 这次结果描述的是【旧引擎】→ 丢弃
            return false;
        }
        inner.paths = paths;
        inner.indexed = indexed;
        inner.error = None;
        true
    }

    /// 建索引失败：记下错误，让前端状态栏能显示「为什么没结果」。
    ///
    /// 【indexed 与 paths 都【不】动】
    ///   旧引擎原封不动，它索引了多少项、索引了哪些目录，状态就该还报什么。
    ///   把 indexed 清 0 会让状态栏说「已索引 0 项」，而结果表里还是旧结果 ——
    ///   那正是「状态与引擎错位」。
    ///
    /// 【在飞计数一定要减】这一趟**真的结束了** → 在飞数 −1。
    ///   （旧模型里「令牌过期 → 什么都不做」会让旧的那趟结束时
    ///     没人能减掉标志 → 标志永远卡在 true。本模型不能重蹈覆辙。）
    ///
    /// 【记不记错，另判】条件是 **`is_latest || is_last_one`**：
    ///   - `is_latest` = 这一趟是**最新**的那一趟
    ///     → 它是「当前用户看到的那趟」的错 → 要记；
    ///   - `is_last_one` = 收掉它之后**没有别的扫描在跑了**
    ///     → 它是**唯一活着的**扫描 → 它的错就是用户该看到的错 → 要记；
    ///   - 两者都不满足 → **不记**（更新的扫描会报自己的错）。
    ///
    ///   【为什么不能只留 `is_latest`】
    ///     A 开始(seq=1) → B 开始(seq=2) → B 成功结束（count 2→1）→ A 失败（count 1→0）
    ///     若只按 `is_latest`：A 的 seq(1) ≠ 最新(2) → **错误被吞** →
    ///     用户看到「没有错误」，但实际有一趟真的失败了 ✗。
    ///     加上 `is_last_one` 之后 → 记下来 ✓。
    ///
    ///   【为什么不能只留 `is_last_one`】
    ///     三趟重叠：A(1) B(2) C(3) → C 失败（count 3→2，`is_latest`=true → 记 C 的错 ✓）
    ///     → B 失败（count 2→1，两个条件都不满足 → 不记）
    ///     → A 失败（count 1→0，`is_last_one`=true → 记 A 的错 ✓）
    ///     → 最后活着的那个的错会被记下来（用户看得到）✓，
    ///     中间的会被更新的一趟覆盖 —— 这是可接受的取舍。
    ///
    ///   若只按 `is_last_one`：C 失败时 count=2 → **不记** →
    ///   **最新的那一趟的错被吞** ✗（用户看到的仍是上一条错，甚至没有）。
    pub fn fail(&self, token: ScanToken, message: String) {
        let mut inner = self.inner.lock().unwrap();
        // ① 【在飞数一定要减】—— 这一趟真的结束了。
        inner.scan_count = inner.scan_count.saturating_sub(1);
        // ② 【记不记错，另判】—— 见上面的说明。
        let is_latest = inner.scan_seq == token.0;
        let is_last_one = inner.scan_count == 0;
        if is_latest || is_last_one {
            inner.error = Some(message);
        }
    }

    /// 【一次锁内】读出全部状态 → 读到的永远**自洽**
    /// （不会出现「新代次 + 旧路径」这种互相矛盾的组合）。
    pub fn snapshot(&self) -> crate::commands::IndexStatus {
        let inner = self.inner.lock().unwrap();
        crate::commands::IndexStatus {
            // 【现算】—— 不存字段 → 不会与计数器不同步。
            scanning: inner.scan_count > 0,
            indexed: inner.indexed,
            paths: inner.paths.clone(),
            error: inner.error.clone(),
            // 【占位值】窗口可见性**这里给不出来** —— 本模块只管索引状态，
            //   不知道窗口在不在屏幕上。
            //   真正知道的人（拿着 AppHandle 的调用点）会覆盖它：
            //   `commands::index_status` 与 `watcher::auto_rescan`。
            //
            //   【为什么占位选 `true` 而不是 `false`】
            //   猜「不可见」→ 前端会**停掉状态轮询** → 状态栏不更新（用户看得见的故障）。
            //   猜「可见」→ 只是多跑一点轮询（性能问题）。
            //   → 宁可性能出问题，也不要功能出问题。
            visible: true,
        }
    }

    /// 【一次锁内】读出「有没有扫描在飞」+「最近一次已完成的扫描的时刻」。
    ///
    /// 【为什么要一次锁内读两个】本模块的规矩是
    ///   「一次加锁读全部 → 读到的永远自洽」（见本文件开头）。
    ///   分两次读会出现「刚扫完、但标志还亮着」这种自相矛盾的组合 ——
    ///   watcher 拿它做判断（扫完没有？），读到矛盾的组合就会做错决定。
    ///
    /// 【返回】`(有扫描在飞, 最近一次已完成的扫描的时刻)`。
    ///
    /// 【签名没变】第一个返回值**仍然是 `bool`**（= 在飞扫描数 > 0）→
    ///   watcher（`watcher.rs`）那边**一行都不用改**。
    pub fn scan_snapshot(&self) -> (bool, Option<Instant>) {
        let inner = self.inner.lock().unwrap();
        (inner.scan_count > 0, inner.last_scan_completed_at)
    }

    /// 最近一次**已完成的扫描**的时刻（测试 / 诊断用）。
    #[cfg(test)]
    pub fn last_scan_completed_at(&self) -> Option<Instant> {
        self.inner.lock().unwrap().last_scan_completed_at
    }

    /// 记下「这一趟扫描成功了，完成于此刻」。
    ///
    /// 【只在成功时调】失败的扫描没更新索引 → 覆盖不了任何变化。
    /// 【谁调】[`run_scan`] / [`run_scan_blocking`] 的**成功分支** ——
    ///   它们是所有扫描的收口（`commands::do_rescan`、启动建索引、换目录重建
    ///   全都走它们）→ 在这里记一次，所有路径都覆盖得到。
    fn note_scan_completed(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.last_scan_completed_at = Some(Instant::now());
    }
}

/// 跑一趟**阻塞的**扫描任务 —— **在飞扫描数的生命周期由这个函数管**。
///
/// 【为什么要有这个】之前 `begin()` 在调用方、收尾（`end_scan`/`fail`）也在调用方，
///   中间任何一条**提前 return** 的路径都会绕过收尾 →
///   在飞数永远回不到 0 → `scanning` 永久卡在 true：
///     - 状态栏永远显示「正在扫描索引…」；
///     - 前端「wasScanning && !s.scanning → 重搜一次」永不成立
///       → 索引就绪后不自动重搜（结果一片空白）。
///   （复审实测发现的 Important-1：`spawn_blocking(...).await` 的 JoinError
///     那条 `?` 就是这么绕过收尾的。
///     旧实现里任何一次 commit 都会顺带清掉卡住的标志；
///     令牌模型把写权限收窄到令牌持有者后，就**没人能救了**。）
///
///   把「begin + 收尾」收进这个函数 → **除了下面列出的那几种情况，没法再绕过**。
///   （那几种：取消 future、闭包 panic —— 见下面的说明。）
///   以后要加新的扫描路径，一律走这里，别自己 begin。
///
/// 【配对不变式】这里的**每一条**退出路径都**恰好**调用一次收尾：
///   `Ok` → `end_scan`，`Err(闭包)` → `fail`，`Err(JoinError)` → `fail`。
///   → `begin` 与收尾**一一配对** → 在飞数不会漏减。
///   **改动时不要在这三条路径之外新增 return。**
///
/// 【还盖不住的一种情况：取消】如果调用方把这个 future **drop 掉**
///   （取消），`spawn_blocking` 里的任务**不会**被中止，但收尾代码
///   也跟着 future 一起被 drop 了 → 在飞数回不到 0 → `scanning` 会卡在 true。
///   **生产不可达**：Tauri 的 `async_runtime::spawn` 立即丢弃 JoinHandle
///   → 任务是 detach 的、不 abort，命令 future 不会在半路被 drop。
///   （复审实测确认过取消确实能绕过，但那条路在 Tauri 里走不到。）
///
/// 【闭包返回 `Result<R, String>`】—— 失败原因由它自己带，
///   本函数负责记进状态（`fail(token, …)`）并把错误原样返回给调用方。
///
/// 【成功时的顺序】先 `end_scan`（在飞数 −1）再把结果返回给调用方；
///   调用方若还要提交状态（`commit_if_current`），在那之后做即可 ——
///   `commit_*` 不动在飞计数，两者互不干扰。
pub async fn run_scan<R, F>(state: &IndexState, f: F) -> Result<R, String>
where
    F: FnOnce() -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let token = state.begin();
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(Ok(v)) => {
            state.end_scan(token);
            // 【成功】记一笔「这一趟扫完了」—— watcher 用它判断
            //   「这次文件变化是不是已经被覆盖了」（见 `scan_snapshot`）。
            state.note_scan_completed();
            Ok(v)
        }
        Ok(Err(e)) => {
            state.fail(token, e.clone());
            Err(e)
        }
        Err(join_err) => {
            // 【线程炸了（panic）】—— 同样必须收尾，
            //   否则在飞数回不到 0、没人能救。
            let msg = join_err.to_string();
            state.fail(token, msg.clone());
            Err(msg)
        }
    }
}

/// 同步版的「增量更新」包装 —— **刻意不记「这一趟扫完了」**。
///
/// 【为什么必须有这个「不一样的版本」】
///   [`run_scan_blocking`] 成功后会调 `note_scan_completed()` ——
///   watcher 靠它判断「这次文件变化是不是已经被某趟扫描覆盖了」
///   （那道「覆盖」闸的**全部意义**就是：用户点一次刷新后，
///   watcher 不要为**同一批**变化再扫一整遍）。
///
///   但**增量更新只覆盖它处理的那几个路径**，不是整棵树 ——
///
///   拿它当「全量扫描」记进去 → 之后的文件变化会被误判成
///   「已经被覆盖」→ **被跳过** → 索引悄悄过期、用户搜不到新文件。
///   → **绝不能混用**：增量更新走这个函数，全量重扫走 `run_scan_blocking`。
///
/// 【其余行为与 `run_scan_blocking` 完全一致】
///   `scanning` 标志的 begin / 收尾（失败也要记错、也要放回标志）。
pub fn run_incremental_blocking<R, F>(state: &IndexState, f: F) -> Result<R, String>
where
    F: FnOnce() -> Result<R, String>,
{
    let token = state.begin();
    match f() {
        Ok(v) => {
            state.end_scan(token);
            // 【刻意**不**调 `note_scan_completed()`】—— 见上面的说明。
            Ok(v)
        }
        Err(e) => {
            state.fail(token, e.clone());
            Err(e)
        }
    }
}

/// 同步版的 [`run_scan`] —— 给**本来就是同步**的调用点用。
///
/// 【谁用】启动路径的 `settings::rebuild_engine_if_absent`
///   （跑在 `lib.rs` 的后台线程里，不是 async；`dev_probe` 的启动线程模拟同理）。
///
/// 【为什么要有同步版】不能让同步函数去 `.await` 一个 async 函数 ——
///   那要把后台线程变成 runtime 线程，徒增复杂度。
///
/// 【盖不住的情况】
///   - **取消**：同步函数没有 future 可 drop → 不适用 ✓
///   - **panic**：`f` 里 panic 会直接 unwind 过去，收尾代码也跳过。
///     **影响有限**：release 的 profile 是 `panic = "abort"`
///     → 整个进程死掉 → 不存在「scanning 卡住」的用户可见状态；
///     dev 下会卡住，但那只影响开发。
///
/// 【配对不变式】同 [`run_scan`]：每条退出路径**恰好**一次收尾。
pub fn run_scan_blocking<R, F>(state: &IndexState, f: F) -> Result<R, String>
where
    F: FnOnce() -> Result<R, String>,
{
    let token = state.begin();
    match f() {
        Ok(v) => {
            state.end_scan(token);
            // 【成功】记一笔「这一趟扫完了」—— 同 `run_scan`。
            state.note_scan_completed();
            Ok(v)
        }
        Err(e) => {
            state.fail(token, e.clone());
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 【begin() 必须把上一次的错误清掉】
    ///
    /// 状态栏的错误提示与「正在扫描索引…」是两个独立分支 ——
    /// 不清的话，重建期间会同时显示「正在扫描…」和一条**过时**的错误。
    #[test]
    fn begin_clears_the_previous_error() {
        let state = IndexState::new();
        let t0 = state.begin();
        state.fail(t0, "invalid argument".to_string());
        assert_eq!(state.snapshot().error.as_deref(), Some("invalid argument"));

        let _t1 = state.begin();
        let snap = state.snapshot();
        assert!(snap.scanning, "begin 之后 scanning 必须是 true");
        assert!(
            snap.error.is_none(),
            "begin 之后不能还留着上一次的错误，实际是 {:?}",
            snap.error
        );
    }

    /// 失败时**不动** paths 与 indexed —— 它们描述的是当前活着的那个引擎。
    #[test]
    fn fail_keeps_paths_and_indexed() {
        let state = IndexState::new();
        state.commit_new_engine(vec!["/tmp/x".to_string()], 42);
        let t = state.begin();
        state.fail(t, "boom".to_string());
        let snap = state.snapshot();
        assert_eq!(snap.paths, vec!["/tmp/x".to_string()]);
        assert_eq!(snap.indexed, 42);
        assert!(!snap.scanning, "fail 之后 scanning 必须是 false");
        assert_eq!(snap.error.as_deref(), Some("boom"));
    }

    /// 成功之后错误被清掉、条目数与路径被提交。
    #[test]
    fn commit_new_engine_commits_and_clears_error() {
        let state = IndexState::new();
        let t0 = state.begin();
        state.fail(t0, "boom".to_string());
        state.commit_new_engine(vec!["/tmp/y".to_string()], 7);
        let snap = state.snapshot();
        assert_eq!(snap.paths, vec!["/tmp/y".to_string()]);
        assert_eq!(snap.indexed, 7);
        assert!(snap.error.is_none());
        assert_eq!(state.engine_snapshot().1, vec!["/tmp/y".to_string()]);
    }

    // ── M1：引擎代次守卫 ──

    /// 【代次守卫 ①】换过引擎之后，**旧代次**的提交必须被拒。
    ///
    /// 【这个 bug 是什么】
    ///   `rescan` 在**开始时**快照了路径，在**结束时**无条件提交。
    ///   若这期间 `reindex` 装上了新引擎并提交了状态，
    ///   rescan 的旧快照会把新状态覆盖掉 ——
    ///   状态栏报「已索引 20001 项」，结果表里却是新引擎的 2 条。
    ///
    /// 【怎么钉住它】不靠时序，靠**代次**：
    ///   换引擎 → 代次 +1 → 旧代次的提交一律拒掉、什么都不改。
    #[test]
    fn stale_commit_is_rejected_after_the_engine_was_replaced() {
        let state = IndexState::new();

        // ① 第一代引擎（大目录）
        state.commit_new_engine(vec!["/old".to_string()], 20001);
        assert_eq!(state.engine_snapshot().0, 1, "装上第一个引擎后代次应是 1");

        // ② rescan 在这一刻快照代次
        let gen1 = state.engine_snapshot().0;

        // ③ 期间 reindex 装上了新引擎（小目录）→ 代次 +1
        state.commit_new_engine(vec!["/new".to_string()], 2);
        assert_eq!(state.engine_snapshot().0, 2, "换引擎后代次应是 2");

        // ④ 跨越了换引擎的旧 rescan 来提交 → 必须被拒
        let accepted = state.commit_if_current(gen1, vec!["/old".to_string()], 20001);
        assert!(
            !accepted,
            "旧代次的提交必须被拒（否则旧结果会覆盖新状态）"
        );

        // ⑤ 状态仍然是【新引擎】那份
        let snap = state.snapshot();
        assert_eq!(snap.paths, vec!["/new".to_string()], "路径不该被旧结果覆盖");
        assert_eq!(snap.indexed, 2, "条目数不该被旧结果覆盖");
        assert!(!snap.scanning, "scanning 不该卡在 true");
    }

    /// 【代次守卫 ②】代次没变时，条件提交**照常提交**（别把正常路径误伤）。
    #[test]
    fn commit_if_current_commits_when_the_generation_matches() {
        let state = IndexState::new();
        state.commit_new_engine(vec!["/a".to_string()], 7);
        let (gen, _paths) = state.engine_snapshot();

        let t = state.begin();
        assert!(state.snapshot().scanning, "begin 之后 scanning 应是 true");

        let accepted = state.commit_if_current(gen, vec!["/a".to_string()], 9);
        assert!(accepted, "代次没变 → 必须提交");

        // 【提交不负责结束扫描】—— scanning 归令牌持有者管
        assert!(
            state.snapshot().scanning,
            "提交状态不该顺带把 scanning 清掉（那是令牌持有者的活）"
        );
        assert!(state.end_scan(t), "当前令牌应该能结束这一趟");
        let snap = state.snapshot();
        assert_eq!(snap.paths, vec!["/a".to_string()]);
        assert_eq!(snap.indexed, 9);
        assert!(!snap.scanning);
        assert!(snap.error.is_none());
    }

    /// 【代次守卫 ③】同一个引擎上重扫（rescan）**不递增**代次 ——
    /// 只有换上新的引擎才 +1。
    #[test]
    fn the_generation_only_grows_when_the_engine_is_replaced() {
        let state = IndexState::new();
        state.commit_new_engine(vec!["/a".to_string()], 7);
        let gen = state.engine_snapshot().0;

        // 同一个引擎上重扫 → 代次不变（用当前代次提交）
        let accepted = state.commit_if_current(gen, vec!["/a".to_string()], 8);
        assert!(accepted);
        assert_eq!(
            state.engine_snapshot().0,
            gen,
            "同一个引擎上重扫不该递增代次（只有换引擎才 +1）"
        );

        // 换引擎 → 代次 +1
        state.commit_new_engine(vec!["/b".to_string()], 3);
        assert_eq!(state.engine_snapshot().0, gen + 1);
    }

    // ── T1：扫描令牌（主权） ──

    /// 【T1】令牌过期 → **不清别人的 scanning**。
    #[test]
    fn an_expired_token_does_not_end_another_scan() {
        let state = IndexState::new();
        let t1 = state.begin();
        assert!(state.snapshot().scanning);
        let t2 = state.begin(); // 第二次扫描接管

        assert!(!state.end_scan(t1), "过期令牌不该结束别人的扫描");
        assert!(state.snapshot().scanning, "scanning 不该被过期令牌清掉");

        assert!(state.end_scan(t2), "当前令牌应该能结束");
        assert!(!state.snapshot().scanning);
    }

    /// 【T2】过期令牌的 `fail()` 也**不该动状态**。
    #[test]
    fn an_expired_token_cannot_record_an_error() {
        let state = IndexState::new();
        let t1 = state.begin();
        let t2 = state.begin();
        state.fail(t1, "过时的错误".to_string());
        let snap = state.snapshot();
        assert!(
            snap.error.is_none(),
            "过期扫描的错误不该记下来（更新的扫描会报自己的）"
        );
        assert!(snap.scanning, "过期扫描的失败不该清掉别人的 scanning");
        // 当前令牌照样能报自己的错
        state.fail(t2, "当前的错误".to_string());
        let snap = state.snapshot();
        assert_eq!(snap.error.as_deref(), Some("当前的错误"));
        assert!(!snap.scanning);
    }

    /// 【T3·单元版】**提交**引擎状态**不动** scanning ——
    /// scanning 的生命周期归令牌持有者管。
    ///
    /// （这条是 T7 反证实验第 4 项的靶子：
    ///   把 `commit_*` 里的 `scanning = false` 加回来 → 这条立刻变红。）
    #[test]
    fn committing_a_new_engine_does_not_clear_the_scanning_flag() {
        let state = IndexState::new();
        let t = state.begin();
        assert!(state.snapshot().scanning);

        // 换引擎 → 提交状态
        state.commit_new_engine(vec!["/a".to_string()], 3);
        assert!(
            state.snapshot().scanning,
            "提交引擎状态不该动 scanning（scanning 归令牌持有者管）"
        );

        // 令牌持有者自己收尾 → 才清
        assert!(state.end_scan(t));
        assert!(!state.snapshot().scanning);
    }

    /// 【T4】`engine_snapshot()` 的代次与路径是**同一时刻**的。
    #[test]
    fn engine_snapshot_returns_the_generation_and_the_paths_together() {
        let state = IndexState::new();
        state.commit_new_engine(vec!["/a".into()], 7);
        let (gen, paths) = state.engine_snapshot();
        assert_eq!(gen, 1);
        assert_eq!(paths, vec!["/a".to_string()]);
    }

    // ── run_scan：scanning 的生命周期收在这一处 ──

    /// 【run_scan ①】线程 **panic**（JoinError）也必须放回 scanning。
    ///
    /// 【这个 bug 是复审实测出来的】之前 `spawn_blocking(...).await` 的 JoinError
    /// 那条 `?` 直接 return → **绕过了令牌收尾** → `scanning` 永久卡在 true：
    ///   - 状态栏永远显示「正在扫描索引…」；
    ///   - 前端「wasScanning && !s.scanning → 重搜一次」永不成立
    ///     → 索引就绪后不自动重搜（结果一片空白）。
    /// （旧实现里任何一次 commit 都会顺带清掉这个卡住的标志；
    ///   令牌模型把写权限收窄到令牌持有者后，就**没人能救了**。）
    #[test]
    fn run_scan_releases_the_flag_even_when_the_task_panics() {
        let state = IndexState::new();
        let res = tauri::async_runtime::block_on(run_scan(&state, || -> Result<(), String> {
            panic!("boom");
        }));
        assert!(res.is_err(), "线程 panic 应该变成 Err");
        assert!(
            !state.snapshot().scanning,
            "线程 panic 也必须放回 scanning（否则状态栏永远「正在扫描索引…」+ 不自动重搜）"
        );
    }

    /// 【run_scan ②】任务自己返回错误 → 错误要记进状态，scanning 也要放回。
    #[test]
    fn run_scan_records_the_error_and_releases_the_flag() {
        let state = IndexState::new();
        let res = tauri::async_runtime::block_on(run_scan(&state, || -> Result<u32, String> {
            Err("boom".to_string())
        }));
        assert_eq!(res.unwrap_err(), "boom");
        let snap = state.snapshot();
        assert!(!snap.scanning, "失败也要放回 scanning");
        assert_eq!(snap.error.as_deref(), Some("boom"), "失败原因要记进状态");
    }

    /// 【run_scan ③】成功 → 放回 scanning，错误被清掉。
    #[test]
    fn run_scan_releases_the_flag_on_success() {
        let state = IndexState::new();
        let t = state.begin();
        state.fail(t, "上一次的错".to_string());
        let res = tauri::async_runtime::block_on(run_scan(&state, || Ok(7u32)));
        assert_eq!(res.unwrap(), 7);
        let snap = state.snapshot();
        assert!(!snap.scanning);
        assert!(snap.error.is_none(), "成功之后不该还留着上一次的错误");
    }

    // ── run_scan_blocking（同步版）：与 async 版对称 ──

    /// 【run_scan_blocking ①】失败 → 错误要记进状态，scanning 也要放回。
    ///
    /// 【为什么必须有这条】复审实测过：把 `run_scan_blocking` 错误分支的
    /// `state.fail(token, e.clone())` 删掉（= 失败时既不放回 scanning、也不记错误），
    /// **全量 32 条测试依然全绿** —— 同步版的失败路径当时**一条覆盖都没有**。
    /// 这正是本次重构要消灭的那类 bug：失败时没人收尾 → `scanning` 永久卡在 true
    /// → 状态栏永远「正在扫描索引…」+ 索引就绪后不自动重搜。
    /// 将来谁「顺手简化」一下这个分支，测试一条都不会红。
    #[test]
    fn run_scan_blocking_records_the_error_and_releases_the_flag() {
        let state = IndexState::new();
        let res = run_scan_blocking(&state, || -> Result<u32, String> {
            Err("boom".to_string())
        });
        assert_eq!(res.unwrap_err(), "boom");
        let snap = state.snapshot();
        assert!(!snap.scanning, "失败也要放回 scanning");
        assert_eq!(snap.error.as_deref(), Some("boom"), "失败原因要记进状态");
    }

    /// 【run_scan_blocking ②】成功 → 放回 scanning，错误被清掉。
    #[test]
    fn run_scan_blocking_releases_the_flag_on_success() {
        let state = IndexState::new();
        let t = state.begin();
        state.fail(t, "上一次的错".to_string());
        let res = run_scan_blocking(&state, || Ok(7u32));
        assert_eq!(res.unwrap(), 7);
        let snap = state.snapshot();
        assert!(!snap.scanning);
        assert!(snap.error.is_none(), "成功之后不该还留着上一次的错误");
    }

    // ── 增量更新**不能**被记成「一趟全量扫描」 ──

    /// 增量更新之后，「最近一次已完成的扫描」必须**仍然是空的**。
    ///
    /// 【为什么这条要钉死】
    ///   watcher 的「覆盖」闸靠这个时刻判断
    ///   「这次文件变化是不是已经被某趟扫描覆盖了」。
    ///   **增量更新只覆盖它处理的那几个路径**，不是整棵树 ——
    ///   把它记成「全量扫描」→ 之后的文件变化会被误判成「已覆盖」
    ///   → **被跳过** → 索引悄悄过期、用户搜不到新文件。
    ///   这类 bug **不会报错**，只会让功能「偶尔不好使」——
    ///   所以必须有一道护栏盯着。
    #[test]
    fn an_incremental_update_does_not_count_as_a_completed_scan() {
        let state = IndexState::new();
        assert!(state.last_scan_completed_at().is_none(), "一开始就该是空的");
        run_incremental_blocking(&state, || -> Result<u32, String> { Ok(7u32) }).unwrap();
        assert!(
            state.last_scan_completed_at().is_none(),
            "增量更新**不能**被记成「一趟全量扫描」—— \
             否则 watcher 的「覆盖」闸会跳过之后的文件变化"
        );
    }

    /// 对照组：全量重扫**确实**要记「已完成」——
    /// 少了它，用户点一次「刷新」之后 watcher 会为同一批变化再扫一整遍。
    #[test]
    fn a_full_rescan_does_count_as_a_completed_scan() {
        let state = IndexState::new();
        run_scan_blocking(&state, || -> Result<u32, String> { Ok(7u32) }).unwrap();
        assert!(
            state.last_scan_completed_at().is_some(),
            "全量重扫必须记下完成时刻（覆盖闸靠它）"
        );
    }

    // ── 「最近一次已完成的扫描」的时刻（Important-1 的地基） ──

    /// 【成功】扫完 → 记下完成时刻。
    ///
    /// 【为什么要有这条】watcher 靠它判断
    ///   「这次文件变化是不是已经被某趟扫描覆盖了」——
    ///   没有它，用户点一次「刷新」之后 watcher 会**再扫一遍**。
    #[test]
    fn a_successful_scan_records_when_it_completed() {
        let state = IndexState::new();
        assert!(
            state.scan_snapshot().1.is_none(),
            "还没扫过时不该有完成时刻"
        );
        let res = run_scan_blocking(&state, || Ok(7u32));
        assert_eq!(res.unwrap(), 7);
        let (scanning, completed_at) = state.scan_snapshot();
        assert!(!scanning, "扫完之后不该还在 scanning");
        assert!(
            completed_at.is_some(),
            "成功的扫描必须记下完成时刻（watcher 靠它判断「已被覆盖」）"
        );
        assert!(
            completed_at.unwrap() <= Instant::now(),
            "完成时刻不该在未来"
        );
    }

    /// 【失败】扫挂 → **不记**完成时刻。
    ///
    /// 【为什么】失败的扫描没更新索引 → 它覆盖不了任何文件变化。
    ///   记上去会让 watcher 以为「这次变化已经被覆盖了」→ 永远扫不到。
    #[test]
    fn a_failed_scan_does_not_record_a_completion_time() {
        let state = IndexState::new();
        let res = run_scan_blocking(&state, || -> Result<u32, String> {
            Err("boom".to_string())
        });
        assert!(res.is_err());
        assert!(
            state.scan_snapshot().1.is_none(),
            "失败的扫描没更新索引，不该记完成时刻"
        );
    }

    /// async 版（`run_scan`）也要记 —— 两条收口路径不能只钉一条。
    #[test]
    fn the_async_scan_also_records_its_completion_time() {
        let state = IndexState::new();
        let res = tauri::async_runtime::block_on(run_scan(&state, || Ok(7u32)));
        assert_eq!(res.unwrap(), 7);
        assert!(
            state.scan_snapshot().1.is_some(),
            "run_scan 成功也必须记完成时刻"
        );
    }

    /// 「有扫描在飞」与「最近一次已完成的扫描」是**一次锁内**读出来的 ——
    /// 读到的两个值必须描述同一个时刻（不能是「刚扫完、但标志还亮着」）。
    #[test]
    fn the_scanning_flag_and_the_completion_time_are_read_together() {
        let state = IndexState::new();
        let res = run_scan_blocking(&state, || Ok(1u32));
        assert_eq!(res.unwrap(), 1);
        let (scanning, completed_at) = state.scan_snapshot();
        assert!(!scanning);
        assert!(completed_at.is_some());

        // 再开一趟扫描（在飞的）→ 标志亮，完成时刻**不变**（上一趟的还是它）
        let token = state.begin();
        let (scanning, completed_at2) = state.scan_snapshot();
        assert!(scanning, "begin 之后标志该是 true");
        assert_eq!(
            completed_at2, completed_at,
            "开新扫描不该改写「上一趟完成于何时」"
        );
        assert!(state.end_scan(token));
    }

    // ── C：重叠扫描（批次 C）—— `scanning` 是「在飞扫描数」 ──

    /// 【C1】两趟重叠，**新的那趟先结束** → `scanning` 必须**还是 true**。
    ///
    /// 【这个 bug 是什么】
    ///   旧模型里 `scanning` 是 bool，语义是「**最新那一趟**结束没有」。
    ///   两趟重叠、**新的那趟先结束**时 → 标志**提前落地**（旧的那趟还在跑）→
    ///   - 状态栏提前从「正在扫描索引…」变回「已索引 N 项」；
    ///   - 前端 `App.vue` 的「wasScanning && !scanning → 重搜一次」**提前成立**
    ///     → 重搜**半成品索引** → 结果表少一批条目，**而且不会自动再刷一次**
    ///     （标志已经落地了，不会再翻转）。
    ///
    /// 【怎么钉住它】不靠时序，靠**计数**：
    ///   两趟都在飞 → 结束任何一趟都还有一趟在跑 → 标志不能落地。
    #[test]
    fn the_newer_scan_ending_first_keeps_the_scanning_flag_on() {
        let state = IndexState::new();
        let t_old = state.begin(); // 在飞 = 1
        let t_new = state.begin(); // 在飞 = 2
        assert!(state.snapshot().scanning);

        // 新的那趟先结束 → 旧的那趟还在跑 → 必须返回 false，且标志不能落地
        assert!(
            !state.end_scan(t_new),
            "新的那趟先结束时还有一趟在跑 → end_scan 必须返回 false"
        );
        assert!(
            state.snapshot().scanning,
            "旧的那趟还在跑 → scanning 必须还是 true（不能提前落地）"
        );

        // 旧的那趟最后结束 → 全完了 → true，标志才落地
        assert!(state.end_scan(t_old), "最后一趟结束 → end_scan 必须返回 true");
        assert!(!state.snapshot().scanning);
    }

    /// 【C2】两趟重叠，**旧的那趟最后结束** → `scanning` 只在**最后一趟**结束时才落地。
    ///
    /// （这条与 C1 是同一场景的两半：C1 盯「提前落地」，C2 盯「该落地时确实落地」——
    ///   后者防的是「计数器减过头」那一类改坏。）
    #[test]
    fn the_scanning_flag_turns_off_only_when_the_last_scan_ends() {
        let state = IndexState::new();
        let t_old = state.begin();
        let t_new = state.begin();

        // 先结束新的那趟 → 还有一趟在跑 → 标志不能落地
        assert!(!state.end_scan(t_new));
        assert!(
            state.snapshot().scanning,
            "还有一趟在跑 → scanning 必须还是 true"
        );

        // 再结束旧的那趟 → 全完了 → 标志落地
        assert!(state.end_scan(t_old));
        assert!(
            !state.snapshot().scanning,
            "最后一趟结束 → scanning 必须变 false"
        );
    }

    /// 【C3】三趟重叠，**乱序结束** → 只要还有一趟在跑，`scanning` 就一直是 `true`。
    ///
    /// 【为什么要三趟】两趟只能区分「先/后」；三趟能区分「中间那趟」——
    ///   乱序结束（新的先走 → 最老的最后走）时，
    ///   任何一趟单独结束都**不能**让标志落地。
    #[test]
    fn with_three_overlapping_scans_the_flag_stays_on_until_the_last_one_ends() {
        let state = IndexState::new();
        let t1 = state.begin(); // 在飞 = 1
        let t2 = state.begin(); // 在飞 = 2
        let t3 = state.begin(); // 在飞 = 3

        // 乱序：中间那趟先走 → 还有两趟在跑
        assert!(!state.end_scan(t2), "还有两趟在跑 → 必须返回 false");
        assert!(state.snapshot().scanning, "还有两趟在跑 → 必须还是 true");

        // 最新的那趟再走 → 还有一趟在跑
        assert!(!state.end_scan(t3), "还有一趟在跑 → 必须返回 false");
        assert!(state.snapshot().scanning, "还有一趟在跑 → 必须还是 true");

        // 最后一趟走 → 全完了
        assert!(state.end_scan(t1), "最后一趟结束 → 必须返回 true");
        assert!(!state.snapshot().scanning);
    }

    /// 【C4】`fail` **也要减在飞计数** —— 失败的那一趟同样「结束了」。
    ///
    /// 【为什么要单独钉】`end_scan` 减了、`fail` 没减 → 标志会**永远卡在 true**：
    ///   状态栏永远「正在扫描索引…」+ 索引就绪后不自动重搜。
    #[test]
    fn fail_also_decrements_the_in_flight_count() {
        let state = IndexState::new();
        let t1 = state.begin(); // 在飞 = 1
        let t2 = state.begin(); // 在飞 = 2

        // 旧的那趟失败 → 它真的结束了 → 在飞 2→1 → 还有一趟在跑
        state.fail(t1, "boom".to_string());
        assert!(
            state.snapshot().scanning,
            "fail 之后还有一趟在跑 → scanning 必须还是 true"
        );

        // 最后一趟也失败 → 在飞 1→0 → 标志落地
        state.fail(t2, "boom2".to_string());
        assert!(
            !state.snapshot().scanning,
            "最后一趟失败后 → scanning 必须变 false"
        );
    }

    /// 【C5】`fail` 的记错条件之一：**「唯一活着的扫描」失败 → 错误要记下来**。
    ///
    /// 【这个场景为什么重要】
    ///   A 开始(seq=1) → B 开始(seq=2) → B **成功**结束（在飞 2→1）
    ///   → A 失败（在飞 1→0）。
    ///   此时 A **不是**最新的那一趟（`is_latest` = false），
    ///   但它是**唯一活着的**扫描（`is_last_one` = true）
    ///   → 它的错就是**用户该看到的那个错** → 必须记。
    ///
    ///   若只按 `is_latest` 判断 → A 的错被**吞掉** →
    ///   用户看到「没有错误」，但实际有一趟真的失败了 ✗。
    #[test]
    fn the_last_surviving_scan_records_its_error() {
        let state = IndexState::new();
        let t_old = state.begin(); // seq = 1
        let t_new = state.begin(); // seq = 2

        // 新的那趟先**成功**结束 → 在飞 2→1
        assert!(!state.end_scan(t_new), "还有一趟在跑 → 必须返回 false");

        // 旧的那趟失败 → 它是唯一活着的扫描 → 错必须记下来
        state.fail(t_old, "唯一活着的扫描失败了".to_string());
        let snap = state.snapshot();
        assert!(
            !snap.scanning,
            "最后一趟失败后 → scanning 必须变 false"
        );
        assert_eq!(
            snap.error.as_deref(),
            Some("唯一活着的扫描失败了"),
            "唯一活着的扫描失败 → 错误必须记下来（它不是最新的那一趟，\
             只靠 is_latest 判断会把这个错吞掉）"
        );
    }

    /// 【C6】`fail` 的记错条件之二：**最新的那一趟失败 → 错误要记**；
    ///       而**过时的扫描**失败（新的还在跑）→ 错误**不记**。
    ///
    /// 【两个方向必须同时成立】
    ///   - 过时的（A，seq=1）失败 → 更新的一趟还在跑 → **不记**
    ///     （更新的会报自己的错；报过时扫描的错只会误导用户）；
    ///   - 最新的（C，seq=3）失败 → 还有一趟（B）在跑 → **要记**
    ///     （它是「当前用户看到的那趟」的错）。
    ///
    ///   注意后半段里 `is_last_one` 是 **false**（在飞 2→1，还剩 B）→
    ///   **只有 `is_latest` 能当记错的理由**。
    ///   → 这条测试正是「记错条件去掉 `is_latest`」那个变异的靶子。
    #[test]
    fn a_stale_failure_is_not_recorded_but_the_latest_one_is() {
        let state = IndexState::new();
        let t_old = state.begin(); // seq = 1
        let _t_mid = state.begin(); // seq = 2
        let t_new = state.begin(); // seq = 3 → 在飞 = 3

        // ① 过时的那趟失败，而更新的还在跑 → **不记**
        state.fail(t_old, "过时的错误".to_string());
        let snap = state.snapshot();
        assert!(
            snap.error.is_none(),
            "过时扫描的错误不该记下来（更新的扫描会报自己的）"
        );
        assert!(
            snap.scanning,
            "还有两趟在跑 → scanning 必须还是 true"
        );

        // ② 最新的那趟失败，而还有一趟在跑 → **要记**
        //    （此时在飞 2→1，`is_last_one` 是 false → 只靠 `is_latest`）
        state.fail(t_new, "最新的错误".to_string());
        let snap = state.snapshot();
        assert_eq!(
            snap.error.as_deref(),
            Some("最新的错误"),
            "最新的扫描失败 → 错误必须记下来（只靠 is_last_one 会把这个错吞掉）"
        );
        assert!(
            snap.scanning,
            "还有一趟在跑 → scanning 必须还是 true"
        );
    }

    /// 【C7】`begin` 会**清掉上一次的错误** —— 行为不变（别回归）。
    ///
    /// 【重叠下的补充】旧的 `begin_clears_the_previous_error` 只覆盖「单趟」；
    ///   这里覆盖「**还有别的扫描在飞**时开新的一趟」：
    ///   错误要清掉，但 `scanning` 必须**还是 true**（新的一趟确实在跑）。
    #[test]
    fn begin_clears_the_previous_error_even_while_another_scan_is_in_flight() {
        let state = IndexState::new();
        let t1 = state.begin(); // 在飞 = 1
        let t2 = state.begin(); // 在飞 = 2
        state.fail(t2, "上一次的错".to_string()); // t2 结束 → 在飞 = 1
        assert_eq!(
            state.snapshot().error.as_deref(),
            Some("上一次的错"),
            "前提：上一次的错确实在状态里"
        );

        // 还有一趟（t1）在飞的时候再开一趟
        let t3 = state.begin(); // 在飞 = 2
        let snap = state.snapshot();
        assert!(
            snap.error.is_none(),
            "begin 之后不能还留着上一次的错误，实际是 {:?}",
            snap.error
        );
        assert!(
            snap.scanning,
            "还有两趟在跑 → scanning 必须还是 true"
        );

        // 收尾两趟 → 全完了
        assert!(!state.end_scan(t3), "还有一趟在跑 → 必须返回 false");
        assert!(state.end_scan(t1), "最后一趟结束 → 必须返回 true");
        assert!(!state.snapshot().scanning);
    }

    /// 【在飞计数不会被重复收尾带成天文数字】
    /// 同一个令牌收尾两次 → 计数**停在 0**（`saturating_sub` 兜底），
    /// **不 panic、不下溢**。
    ///
    /// 【这条兜底的边界】它只防「下溢」，**防不了「重复收尾让计数少 1」** ——
    ///   若两趟在飞时其中一趟被收尾两次 → 计数会提前掉到 0 →
    ///   标志提前落地（又回到老 bug）。
    ///   → 防重复收尾要靠「每条退出路径恰好一次收尾」那条不变式
    ///   （由 `run_scan` / `run_scan_blocking` 保证）。
    #[test]
    fn a_double_end_scan_does_not_underflow_the_counter() {
        let state = IndexState::new();
        let t = state.begin();
        assert!(state.end_scan(t), "第一次收尾 → 全完了 → true");
        // 误调第二次 → 计数停在 0（不 panic、不下溢成天文数字）
        state.end_scan(t);
        assert!(
            !state.snapshot().scanning,
            "重复收尾不该把计数带成天文数字（否则 scanning 会永远 true）"
        );
    }
}
