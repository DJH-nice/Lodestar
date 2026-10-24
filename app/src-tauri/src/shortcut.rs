//! 全局快捷键 —— 在任何 app 里按一下就能唤起 Lodestar。
//!
//! 【为什么注册失败必须让用户知道】
//!   `⌥⌘Space` 很可能被别的软件占着。注册失败如果**静默**，
//!   用户按了没反应，还以为功能坏了 —— 所以要把错误显示出来。

use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// 默认快捷键。
pub const DEFAULT_SHORTCUT: &str = "Option+Cmd+Space";

/// 快捷键的**运行时真相** —— 「设置 = 实际生效的键」这条规则的依据。
///
/// 【为什么要有这一份状态】
///   `settings.json` 里存的是**用户的偏好**
///   （「我想用 ⌥⌘Space」），而**实际生效**的是**本进程注册着**的那个键。
///   两者会分叉 —— 最典型的一条：
///   **启动时** `⌥⌘Space` 被别的软件占着 → 注册失败 →
///   设置里**一个字都没改**（仍然是 `Option+Cmd+Space`）→
///   面板若直接显示设置里的值，就等于骗人说「它已生效」。
///   （`commands.rs` 里那条硬规则「设置 = 实际生效的键」
///     只管住了「改键」那条路径，**启动路径**没走它。）
///
/// 【为什么不能每次都去问插件】
///   插件的 `is_registered()` 只能问「**某一个**键在不在」，
///   问不出「本进程到底注册了哪些键」——
///   而面板要显示的恰恰是「此刻生效的是哪个」。
///   → 由 [`apply`]（唯一知道结果的地方）把真相记下来。
///
/// 【为什么还要交叉核对】
///   见 [`effective_shortcut`]：维护的状态与插件的注册表**对一遍**，
///   两边不一致时以**插件**为准（那是 OS 层面的事实）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortcutRuntime {
    /// 本进程**实际注册着**的键（`None` = 一个都没有生效）。
    pub effective: Option<String>,
    /// 最近一次出问题的原因（成功注册后清空）。
    pub error: Option<String>,
}

/// 进程级的运行时状态 —— **一个进程一份**。
///
/// 【为什么是 `static` 而不是 Tauri 的 managed state】
///   它描述的是「**本进程**注册了什么」，
///   而快捷键注册表本身就是进程级的（插件自己也是进程级的一份）。
///   放成 managed state 的话，单测里构造的 mock app 没 manage 它 →
///   `app.state::<…>()` 直接 panic。
///   → 放成模块内的 static：与 app 无关，也不会 panic。
static RUNTIME: Mutex<ShortcutRuntime> = Mutex::new(ShortcutRuntime {
    effective: None,
    error: None,
});

/// 取运行时状态 —— **中毒了也继续跑**。
///
/// 【为什么要这样】
///   中毒只说明「某个持锁者 panic 过」，
///   而这里持锁期间只读写两个 `Option<String>` —— 数据本身是好的。
///   `.unwrap()` → 中毒时 panic → **整个快捷键路径死掉**
///   （而这条路径正管着「唤起 Lodestar」这个主入口）。
///   → 取回内值继续跑。
fn runtime() -> MutexGuard<'static, ShortcutRuntime> {
    match RUNTIME.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    }
}

/// 记下「此刻生效的是哪个键」+「有没有出问题」。
fn store_runtime(rt: ShortcutRuntime) {
    *runtime() = rt;
}

/// 一次**成功**的 `apply` 之后，运行时状态该变成什么样 —— **纯函数**（好测）。
///
/// 【为什么「成功」就是「生效的 = spec」】
///   - `Register(s)` 成功       → s 生效
///   - `Replace` 成功           → new 生效、旧键注销掉了
///   - `Unregister(o)` 成功     → 一个都不生效（spec 是 `None`）
///   - `Nothing`（同一个键还在）→ 那个键继续生效
///   → 统一就是「= spec」。
pub fn runtime_after_success(spec: Option<&str>) -> ShortcutRuntime {
    ShortcutRuntime {
        effective: spec.map(|s| s.to_string()),
        error: None,
    }
}

/// 一次「**注册失败 / 规格非法**」的 `apply` 之后 —— **纯函数**（好测）。
///
/// 【关键】`prev.effective` **一个字都不改** ——
///   这两类失败的后果是「**什么都没变**」（旧的键原封不动还在生效）。
///   只有 `error` 变了。
pub fn runtime_after_failure(prev: &ShortcutRuntime, error: String) -> ShortcutRuntime {
    ShortcutRuntime {
        effective: prev.effective.clone(),
        error: Some(error),
    }
}

/// 一次「**注销失败**」的 `apply` 之后 —— **纯函数**（好测）。
///
/// 【这一条不一样】
///   新键**已经生效了** → `effective` 要更新成新键；
///   但「旧键可能还在唤起」这件事要记进 `error` ——
///   面板得把这句话说给用户听（托盘那边说的是「⚠️ 旧快捷键未注销」）。
pub fn runtime_after_unregister_failure(
    effective: Option<&str>,
    error: String,
) -> ShortcutRuntime {
    ShortcutRuntime {
        effective: effective.map(|s| s.to_string()),
        error: Some(error),
    }
}

/// 本进程**实际注册着**的快捷键 —— **与插件的注册表交叉核对过**。
///
/// 【为什么要交叉核对】
///   维护的状态是「`apply` 执行完之后」的快照；
///   插件的注册表是**当下**的事实。
///   两者不一致时（比如系统在背后把热键收了），
///   以**插件**为准 —— 它才代表「此刻按下去会不会唤起」。
pub fn effective_shortcut<R: Runtime>(app: &AppHandle<R>) -> Option<String> {
    let spec = runtime().effective.clone()?;
    // 问插件：「这个键真的还注册着吗」
    match spec.parse::<Shortcut>() {
        Ok(sc) if app.global_shortcut().is_registered(sc) => Some(spec),
        // 状态说它生效、插件说没有 → **以插件为准**（它才决定按下去会不会唤起）
        _ => None,
    }
}

/// 最近一次注册失败的原因（`None` = 一切正常）。
pub fn runtime_error() -> Option<String> {
    runtime().error.clone()
}

/// **仅测试用**：把运行时状态清空。
///
/// 【为什么需要】
///   运行时状态是**进程级**的一份（`static`）——
///   而 cargo test 默认在**同一个进程**里并行跑所有用例。
///   任何断言它内容的用例都得先清空它，否则会被别的用例污染
///   （谁先跑完谁说了算 → 偶发红）。
#[cfg(test)]
pub(crate) fn reset_runtime() {
    store_runtime(ShortcutRuntime::default());
}

/// 快捷键操作失败 —— **区分「什么都没变」和「状态已经变了」两种后果**。
///
/// 【为什么不能只返回 String】
///   ① 「注册失败」= **什么都没变**：旧键还在生效，设置也不该改 → 用户可以重试
///   ② 「注销失败」= **状态已经变了** → 设置**应该**跟着改，但要警告
///   两者混成一句话 → 面板要么骗人说「操作失败」（实际新键已生效），
///   要么骗人说「已切换」（实际旧键还在）。
///
/// 【为什么「注销失败」要带上「此刻生效的是哪个键」】
///   同一个 `UnregisterFailed`，**生效的键完全不同**：
///     换键（A→B，B 注册成功、注销 A 失败）→ 生效的是 **B**（A 也可能还在）
///     关掉（注销 A 失败）                    → 生效的是 **A**（它没被注销掉！）
///   两种情况只有 `apply` 知道 —— **让错误携带真相，而不是让调用方猜**。
///   （猜错的后果：面板显示「未设置」，而 A 其实还在唤起 Lodestar。）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutError {
    /// 规格非法（格式错 / 没有修饰键）—— 什么都没变
    Invalid(String),
    /// 注册新键失败（被别的软件占用）—— 什么都没变，**旧键仍然有效**
    RegisterFailed {
        /// 想注册的那个键（要告诉用户是**哪个**键失败了）
        spec: String,
        /// 底层的原因
        detail: String,
    },
    /// 注销旧键失败 —— **此刻真正生效的是哪个键，由 `apply` 算出来**
    UnregisterFailed {
        /// 底层的原因
        detail: String,
        /// **此刻真正生效**的快捷键。`None` = 确实没有键生效。
        effective: Option<String>,
        /// 可能**也**还在生效的另一个键
        /// （换键时 = 没注销掉的旧键；关掉时 = 无）。
        also_active: Option<String>,
    },
}

impl ShortcutError {
    /// 变成给用户看的一句话。
    pub fn message(&self) -> String {
        match self {
            // 【为什么不再加一遍「快捷键格式非法：」前缀】
            //   `validate` 返回的**已经是完整的一句话**（有的自带这个前缀）——
            //   再加一遍就成了「快捷键格式非法：快捷键格式非法：…」。
            ShortcutError::Invalid(d) => d.clone(),
            ShortcutError::RegisterFailed { spec, detail } => {
                format!("快捷键「{spec}」注册失败（可能已被别的软件占用）：{detail}")
            }
            // 【为什么分两种语境说】
            //   「换键」：新键生效了、旧键没注销掉 → **两个都会唤起**
            //   「关掉」：没有新键、旧键没注销掉 → **它就是生效的那个，压根没关掉**
            //   （这两句混成一句，就有一半是错的。）
            ShortcutError::UnregisterFailed {
                detail,
                effective,
                also_active,
            } => match (effective, also_active) {
                (Some(eff), Some(also)) => format!(
                    "旧快捷键「{also}」注销失败（它可能仍会唤起 Lodestar，与新快捷键「{eff}」同时生效）：{detail}"
                ),
                (Some(eff), None) => format!(
                    "快捷键「{eff}」注销失败（它仍会唤起 Lodestar —— 快捷键**没有被关掉**）：{detail}"
                ),
                (None, _) => {
                    format!("快捷键注销失败（它可能仍会唤起 Lodestar）：{detail}")
                }
            },
        }
    }
}

impl std::fmt::Display for ShortcutError {
    /// 【为什么要有 Display】托盘警告项要拼进一句短话里
    ///   （`format!("⚠️ {e}")`）—— 有了 Display 就不用到处手写 `e.message()`。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

/// 校验一个快捷键规格 —— **纯函数**（好测）。
///
/// 【为什么要「必须有修饰键」】
///   纯 `Space` / 纯 `K` 会在你正常打字时误触发唤起。
pub fn validate(spec: &str) -> Result<(), String> {
    let shortcut: Shortcut = spec
        .parse()
        .map_err(|e| format!("快捷键格式非法：{e}"))?;
    if shortcut.mods.is_empty() {
        return Err("必须带修饰键（⌘ / ⌥ / ⌃ / ⇧），否则会和正常打字冲突".to_string());
    }
    Ok(())
}

/// 换快捷键时的动作 —— **纯函数**（好测）。
///
/// 【为什么要有它】`apply` 的真正难点是**顺序**：
///   新键注册失败时，旧的必须**原封不动**还在。
///   顺序错了 → 用户既没新键也没旧键 → 唤起入口全没了。
///   「该做什么」抽成纯函数（`plan`），
///   「按什么顺序做」抽成纯函数（[`steps`]）—— 两半都可测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 什么都不用做（键没变，而且确实还注册着）。
    Nothing,
    /// 注册这个键。
    Register(String),
    /// 注册这个键，**注册成功后**再注销那个旧的。
    Replace { new: String, old: Option<String> },
    /// 注销这个键。
    Unregister(String),
}

/// 算动作。
///
/// - `old` —— 之前注册的是哪个
/// - `spec` —— 现在要注册哪个
/// - `old_still_registered` —— 旧的**是不是真的还注册着**
///   （上次注册失败过的话，设置里存着它、但实际没注册）
pub fn plan(old: Option<&str>, spec: Option<&str>, old_still_registered: bool) -> Action {
    if old == spec {
        return match spec {
            None => Action::Nothing,
            Some(s) => {
                if old_still_registered {
                    Action::Nothing
                } else {
                    // 存的键没真的注册着 → 重试注册
                    Action::Register(s.to_string())
                }
            }
        };
    }
    match (spec, old) {
        // 【第一次注册】之前**没注册过** → 直接注册。
        //
        // 【为什么要单独拎出来】`Replace` 的语义是「换键」——
        //   第一次注册没有旧键可换，不该走那条路
        //   （`Replace { old: None }` 跑起来效果一样，但说谎了：
        //    一眼看去像是「有个旧键被换掉了」）。
        (Some(s), None) => Action::Register(s.to_string()),
        (Some(s), o) => Action::Replace {
            new: s.to_string(),
            // 【为什么要看 `old_still_registered`】（终审 M-9）
            //   设置里写着 A、但 A 其实**没注册**（上次启动注册失败过）时，
            //   换键**不该**去注销 A —— 它压根没注册着，注销是白跑一趟。
            //   （今天靠 macOS 端「注销没注册过的键返回 Ok」兜住，无害；
            //     但那是**靠下游幂等性**兜的语义漏洞，
            //     而且 `is_registered` 这个实参在 Replace 路径上白算了。）
            //   → 只在旧键**确实还注册着**时才去注销它。
            old: if old_still_registered {
                o.map(|x| x.to_string())
            } else {
                None
            },
        },
        (None, Some(o)) => Action::Unregister(o.to_string()),
        (None, None) => Action::Nothing,
    }
}

/// 一个执行步骤 —— **顺序本身就是数据，于是顺序可测**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// 注册这个快捷键。**失败即中止后面的步骤**。
    Register(String),
    /// 注销这个快捷键。
    Unregister(String),
}

/// 把一个 Action 展开成**有序**的步骤序列。
///
/// 【为什么顺序要变成数据】`apply` 里手写的 if/else 顺序是测不出来的：
///   把「先注册后注销」改成「先注销后注册」，没有任何测试会红。
///   顺序一旦变成 `Vec<Step>`，它就是可断言的返回值。
///
/// 【不变式自动成立】`apply` 顺序执行这些步骤，`Register` 失败就 `return Err`
///   → 后面的 `Unregister` **自然不会执行** → 旧的键原封不动还在。
pub fn steps(action: &Action) -> Vec<Step> {
    match action {
        Action::Nothing => vec![],
        Action::Register(s) => vec![Step::Register(s.clone())],
        Action::Replace { new, old } => {
            let mut v = vec![Step::Register(new.clone())];
            if let Some(o) = old {
                v.push(Step::Unregister(o.clone()));
            }
            v
        }
        Action::Unregister(o) => vec![Step::Unregister(o.clone())],
    }
}

/// **应用**一个快捷键规格。
///
/// - `old` = 之前注册的是哪个（`None` = 之前没注册）
/// - `spec` = 现在要注册哪个（`None` = 注销掉、不注册）
///
/// 【顺序：先注册新的、成功了再注销旧的】
///   如果先注销旧的、新的又注册失败（被别的软件占着），
///   用户就**既没有新键、也没有旧键** —— 唤起入口全没了。
///   先注册新的：成功 → 再注销旧的；
///   失败 → 旧的**原封不动**还在，一切照旧（只是没换成新键）。
///
/// 【顺序不是写在这里的，而是 `steps()` 给的】
///   顺序写死在这个函数的 if/else 里就**测不出来**：
///   把它改成「先注销后注册」，没有任何测试会变红。
///   → 顺序由纯函数 [`steps`] 产出（可断言），
///     这里只负责**按序执行**。
///
/// 【错误为什么是结构化的（[`ShortcutError`】）】
///   「注册失败」= 什么都没变（旧键还在）；
///   「注销失败」= 状态已经变了 —— 而**此刻生效的是哪个键**，
///   由这个函数算出来放进错误里（换键 = 新键；关掉 = 没注销掉的旧键）。
///   调用方（`commands::set_global_shortcut`）只照着错误里的 `effective` 写设置，
///   不用自己猜「用户到底请求了什么」。
pub fn apply<R: Runtime>(
    app: &AppHandle<R>,
    old: Option<&str>,
    spec: Option<&str>,
) -> Result<(), ShortcutError> {
    // ① 校验新的 —— 格式错 / 没修饰键 → 明确报错，**不动现有注册**
    if let Some(s) = spec {
        if let Err(detail) = validate(s) {
            let e = ShortcutError::Invalid(detail);
            // 【原因要记下来】—— 面板要能说清「为什么没生效」。
            //   注意：这里**只**改 error，「生效的是哪个键」一个字都不动
            //   （什么都没变）。
            let prev = runtime().clone();
            store_runtime(runtime_after_failure(&prev, e.message()));
            return Err(e);
        }
    }
    // ② 旧的到底还注册着吗？
    let old_still_registered = match old {
        Some(o) => match o.parse::<Shortcut>() {
            Ok(sc) => app.global_shortcut().is_registered(sc),
            Err(_) => false,
        },
        None => false,
    };
    // ③ 按计划执行 —— **顺序由 `steps()` 决定**。
    let action = plan(old, spec, old_still_registered);
    // 【本次调用里【真的注册上了】的键】——
    //   注销失败时要报「此刻生效的是哪个键」，靠它区分两条路径：
    //     换键（新键注册上了）→ 生效的是**新键**
    //     关掉（没有新键）    → 生效的是**没注销掉的那个旧键**
    let mut registered_now: Option<String> = None;
    // 注销失败（原因，没注销掉的那个键）
    let mut unregister_failed: Option<(String, String)> = None;
    for step in steps(&action) {
        match step {
            // 【关键】注册失败 → **立即返回 Err** →
            //   后面的步骤（注销旧的）**不会执行** →
            //   旧的键原封不动还在。
            Step::Register(s) => {
                if let Err(e) = register(app, &s) {
                    // 【什么都没变】→ 「生效的是哪个键」一个字都不动，
                    //   只把**原因**记下来（面板要能说清为什么没生效）。
                    let prev = runtime().clone();
                    store_runtime(runtime_after_failure(&prev, e.message()));
                    return Err(e);
                }
                // 注册**真的成功了** → 它此刻是生效的那个键
                registered_now = Some(s.clone());
            }
            Step::Unregister(o) => {
                if let Err(detail) = unregister(app, &o) {
                    eprintln!("[lodestar] 注销旧快捷键失败（旧的可能仍然生效）: {detail}");
                    // 【为什么不回滚】回滚 = 把新键也注销掉 →
                    //   「既没新键也没旧键」→ 唤起入口全丢。
                    //   → 只**如实上报**，不动已经生效的新键。
                    unregister_failed.get_or_insert((detail, o.clone()));
                }
            }
        }
    }
    match unregister_failed {
        // 注销失败**必须上报**：吞掉它 = 面板显示「已关闭」，
        //   而热键其实还注册着、还会唤起应用。
        None => {
            // 【成功 → 记下「此刻生效的是哪个键」】
            //   统一就是「= spec」（理由见 `runtime_after_success`）。
            //   → 面板显示的是**实际生效**的那个，不是设置里存的那个。
            store_runtime(runtime_after_success(spec));
            Ok(())
        }
        Some((detail, still_active)) => {
            // 【此刻真正生效的是哪个键】—— **只有这里知道**
            //   （纯函数、可测：见 `effective_after_unregister_failure`）
            let (effective, also_active) =
                effective_after_unregister_failure(registered_now.as_deref(), &still_active);
            // 【记下问题】—— 面板要能说清「为什么没按预期生效」。
            let err = ShortcutError::UnregisterFailed {
                detail,
                effective: effective.clone(),
                also_active: also_active.clone(),
            };
            store_runtime(runtime_after_unregister_failure(
                effective.as_deref(),
                err.message(),
            ));
            Err(err)
        }
    }
}

/// 注销失败时 —— **此刻真正生效的是哪个键**。
///
/// 【为什么抽成纯函数】这是本模块最容易被改坏的一处：
///   算错 → 面板显示「未设置」，而旧键其实**还在唤起 Lodestar**（骗人）。
///   它必须**可测**，而且不能依赖 `AppHandle`（单测里没有真插件）。
///
/// - `registered_now` —— 本次调用里**真的注册上了**的键
///   （换键时 = 新键；「关掉快捷键」时 = 无）
/// - `still_active` —— **没注销掉**的那个键（它还在生效）
///
/// 返回 `(effective, also_active)`：
///   - `effective` —— **此刻真正生效**的快捷键
///   - `also_active` —— 可能**也**还在生效的另一个键（关掉那条路径上没有）
pub fn effective_after_unregister_failure(
    registered_now: Option<&str>,
    still_active: &str,
) -> (Option<String>, Option<String>) {
    match registered_now {
        // 换键：新键真的注册上了 → 它是「生效的那个」；
        //   旧键没注销掉 → 「可能也还在」（两个都会唤起）
        Some(new) => (Some(new.to_string()), Some(still_active.to_string())),
        // 关掉快捷键：没有新键 → 没注销掉的那个**就是**生效的那个
        //   （报成 `None` 就是在骗人 —— 面板会说「未设置」，而它还在唤起！）
        None => (Some(still_active.to_string()), None),
    }
}

/// 注册一个快捷键并装上唤起 handler。
///
/// 【错误怎么分类】
///   - 规格解析不了 → [`ShortcutError::Invalid`]（什么都没变）
///   - 插件注册失败（被别的软件占着）→ [`ShortcutError::RegisterFailed`]
///     （**旧键原封不动还在** —— 步骤顺序保证了这一点）
fn register<R: Runtime>(app: &AppHandle<R>, spec: &str) -> Result<(), ShortcutError> {
    let shortcut: Shortcut = spec
        .parse()
        .map_err(|e| ShortcutError::Invalid(format!("快捷键格式非法：{e}")))?;
    app.global_shortcut()
        .on_shortcut(shortcut, |app, _sc, event| {
            // 【只在「按下」时唤起】——
            //   插件的回调在**按下和松开**时都会来一次。
            //   松开也唤起 → 用户「按下去唤起一次、手松开又唤起一次」，
            //   前端会收到两遍「已唤起」事件。
            if event.state == ShortcutState::Pressed {
                crate::lifecycle::wake(app);
            }
        })
        .map_err(|e| ShortcutError::RegisterFailed {
            spec: spec.to_string(),
            detail: e.to_string(),
        })
}

/// 注销一个快捷键。
///
/// 【为什么失败要往上传】
///   吞掉它（`let _ = ..`）= 面板显示「已关闭」，
///   而热键其实还注册着、还会唤起应用 —— 用户以为关掉了，其实没关。
///   → 错误往上传给 [`apply`]，由它如实上报。
///
/// 【为什么不回滚】
///   回滚 = 把刚注册好的新键也注销掉 →
///   「既没新键也没旧键」→ 唤起入口全丢（比不回滚更糟）。
///   → 只上报，**不动**已经生效的新键。
///
/// 【返回的是「原因」而不是一句话】
///   上层要把它装进 [`ShortcutError::UnregisterFailed`]，
///   而那条错误的文案是**中性**的（两条路径都要读得通）——
///   细节放在这里，前缀交给上层统一处理。
fn unregister<R: Runtime>(app: &AppHandle<R>, spec: &str) -> Result<(), String> {
    let shortcut: Shortcut = spec
        .parse()
        .map_err(|e| format!("旧快捷键规格非法（{spec}），无法注销：{e}"))?;
    app.global_shortcut()
        .unregister(shortcut)
        .map_err(|e| format!("注销失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shortcut_is_option_cmd_space() {
        assert_eq!(DEFAULT_SHORTCUT, "Option+Cmd+Space");
    }

    #[test]
    fn a_valid_shortcut_passes_validation() {
        assert!(validate("Option+Cmd+Space").is_ok());
        assert!(validate("CmdOrCtrl+Shift+F12").is_ok());
        assert!(validate("Control+Option+KeyA").is_ok());
    }

    #[test]
    fn a_shortcut_without_modifiers_is_rejected() {
        let err = validate("Space").unwrap_err();
        assert!(err.contains("修饰键"), "错误信息要说明为什么要修饰键，实际是：{err}");
    }

    #[test]
    fn a_garbage_shortcut_is_rejected() {
        assert!(validate("这不是快捷键").is_err());
        assert!(validate("").is_err());
    }

    // ---- 注册顺序 —— 「新键失败时旧的必须原封不动」这条不变式 ----

    #[test]
    fn first_registration_just_registers() {
        assert_eq!(
            plan(None, Some("Option+Cmd+Space"), false),
            Action::Register("Option+Cmd+Space".to_string())
        );
    }

    #[test]
    fn changing_the_key_registers_the_new_one_first() {
        assert_eq!(
            plan(
                Some("Option+Cmd+Space"),
                Some("CmdOrCtrl+K"),
                true
            ),
            Action::Replace {
                new: "CmdOrCtrl+K".to_string(),
                old: Some("Option+Cmd+Space".to_string()),
            }
        );
    }

    #[test]
    fn the_same_key_still_registered_does_nothing() {
        assert_eq!(
            plan(
                Some("Option+Cmd+Space"),
                Some("Option+Cmd+Space"),
                true
            ),
            Action::Nothing
        );
    }

    #[test]
    fn the_same_key_not_registered_is_retried() {
        // 上次注册失败过 → 设置里存着这个键、但实际没注册。
        // 这时用户「重新录一遍同样的键」必须**重试注册**，
        // 而不是什么都不做（否则用户永远修不好）。
        assert_eq!(
            plan(
                Some("Option+Cmd+Space"),
                Some("Option+Cmd+Space"),
                false
            ),
            Action::Register("Option+Cmd+Space".to_string())
        );
    }

    #[test]
    fn clearing_unregisters_the_old_key() {
        assert_eq!(
            plan(Some("Option+Cmd+Space"), None, true),
            Action::Unregister("Option+Cmd+Space".to_string())
        );
    }

    #[test]
    fn replacing_a_key_that_is_not_registered_does_not_unregister_it() {
        // 【终审 M-9】设置里写着 A，但 A 其实**没注册**（上次启动注册失败过）→
        //   换键时**不该**去注销 A（它压根没注册着）。
        //   （以前无条件带上 old → 白跑一次注销；
        //     今天靠 macOS 端「注销没注册过的键返回 Ok」兜住 —— 靠下游幂等性兜的漏洞。）
        assert_eq!(
            plan(Some("Option+Cmd+Space"), Some("CmdOrCtrl+K"), false),
            Action::Replace {
                new: "CmdOrCtrl+K".to_string(),
                old: None,
            },
            "旧键没注册着 → 不该再去注销它"
        );
    }

    #[test]
    fn nothing_registered_and_nothing_wanted_is_a_no_op() {
        assert_eq!(plan(None, None, false), Action::Nothing);
    }

    // ---- 执行**顺序** —— 「新键失败时旧的必须原封不动」这条不变式 ----
    //
    // 【为什么要单独测顺序】`plan` 只回答「做什么」，
    //   回答不了「按什么顺序做」。
    //   顺序要是只活在 `apply` 的 if/else 里，
    //   把它改成「先注销后注册」不会有任何测试变红 ——
    //   而线上后果是「换键失败 → 旧键也没了 → 唤起入口全丢」。
    //   → 顺序必须是**可断言的返回值**。

    #[test]
    fn replacing_registers_the_new_key_before_unregistering_the_old_one() {
        assert_eq!(
            steps(&Action::Replace {
                new: "A".into(),
                old: Some("B".into())
            }),
            vec![Step::Register("A".into()), Step::Unregister("B".into())],
            "换键必须**先注册新的** —— 顺序反了，新键注册失败时旧键已经没了"
        );
    }

    #[test]
    fn replacing_with_no_old_key_has_only_the_register_step() {
        assert_eq!(
            steps(&Action::Replace {
                new: "A".into(),
                old: None
            }),
            vec![Step::Register("A".into())]
        );
    }

    #[test]
    fn registering_is_a_single_step() {
        assert_eq!(
            steps(&Action::Register("A".into())),
            vec![Step::Register("A".into())]
        );
    }

    #[test]
    fn unregistering_is_a_single_step() {
        assert_eq!(
            steps(&Action::Unregister("A".into())),
            vec![Step::Unregister("A".into())]
        );
    }

    #[test]
    fn doing_nothing_has_no_steps() {
        assert_eq!(steps(&Action::Nothing), Vec::<Step>::new());
    }

    // ---- 错误分类 —— 「什么都没变」 vs 「状态已经变了」 ----
    //
    // 【为什么要单独测】`apply` 的 `Err` 现在有**两种后果完全不同**的含义：
    //   ① 注册失败 = **什么都没变**（旧键还在生效）→ 设置**不能改**
    //   ② 注销失败 = **状态变了** → 设置**要改** + 警告
    // 混成一句话 → 设置面板要么骗人说「操作失败」（实际新键已生效），
    // 要么骗人说「已切换」（实际旧键还在）。

    fn all_errors() -> Vec<ShortcutError> {
        vec![
            ShortcutError::Invalid("必须带修饰键".to_string()),
            ShortcutError::RegisterFailed {
                spec: "Cmd+K".to_string(),
                detail: "已被别的软件占用".to_string(),
            },
            ShortcutError::UnregisterFailed {
                detail: "注销失败".to_string(),
                effective: Some("Cmd+K".to_string()),
                also_active: Some("Option+Cmd+Space".to_string()),
            },
        ]
    }

    #[test]
    fn every_error_has_a_non_empty_message() {
        for e in all_errors() {
            assert!(!e.message().is_empty(), "错误必须有话可说：{e:?}");
        }
    }

    #[test]
    fn the_register_message_says_which_key_failed() {
        let msg = ShortcutError::RegisterFailed {
            spec: "Cmd+K".to_string(),
            detail: "已被别的软件占用".to_string(),
        }
        .message();
        assert!(
            msg.contains("Cmd+K"),
            "用户得知道是**哪个键**没注册上，实际：{msg}"
        );
        assert!(
            msg.contains("已被别的软件占用"),
            "底层原因也要带出来，实际：{msg}"
        );
    }

    #[test]
    fn the_invalid_message_is_not_prefixed_twice() {
        // 【为什么】`validate` 的错误消息**已经是完整的一句话**
        //   （有的自带「快捷键格式非法：」这个前缀）——
        //   再加一遍就成了「快捷键格式非法：快捷键格式非法：…」。
        let msg = ShortcutError::Invalid("快捷键格式非法：xxx".to_string()).message();
        assert_eq!(
            msg.matches("快捷键格式非法").count(),
            1,
            "前缀不该出现两次，实际：{msg}"
        );
    }

    // ---- 「此刻生效的是哪个键」—— 面板显示什么，全看这里 ----

    #[test]
    fn clearing_a_shortcut_whose_unregister_fails_keeps_the_old_key_as_effective() {
        // 【路径 ③】用户点「清除」→ 注销失败 → **旧键还在生效**。
        //   报成 `None` = 面板显示「未设置」→ 用户以为关掉了，其实还在唤起。
        let (effective, also_active) =
            effective_after_unregister_failure(None, "Option+Cmd+Space");
        assert_eq!(
            effective.as_deref(),
            Some("Option+Cmd+Space"),
            "注销失败 → 旧键**还在生效**，effective 不能是 None"
        );
        assert_eq!(
            also_active, None,
            "「关掉快捷键」这条路径上没有「另一个也生效的键」"
        );
    }

    #[test]
    fn replacing_a_shortcut_whose_unregister_fails_reports_the_new_key_as_effective() {
        // 【路径 ②】换键 A→B：B 注册成功、注销 A 失败 → **生效的是 B**，A 也可能还在。
        let (effective, also_active) =
            effective_after_unregister_failure(Some("Cmd+K"), "Option+Cmd+Space");
        assert_eq!(effective.as_deref(), Some("Cmd+K"), "新键才是生效的那个");
        assert_eq!(
            also_active.as_deref(),
            Some("Option+Cmd+Space"),
            "旧键没注销掉 → 它「可能也还在」"
        );
    }

    #[test]
    fn the_unregister_message_says_the_shortcut_was_not_turned_off() {
        // 「关掉快捷键」那条路径：没有新键 → 文案不能说「新键」，
        //   还要说清**它没被关掉**。
        let msg = ShortcutError::UnregisterFailed {
            detail: "注销失败".to_string(),
            effective: Some("Option+Cmd+Space".to_string()),
            also_active: None,
        }
        .message();
        assert!(
            msg.contains("Option+Cmd+Space"),
            "要说清是**哪个键**没关掉，实际：{msg}"
        );
        assert!(
            msg.contains("没有被关掉"),
            "要说清它还在生效，实际：{msg}"
        );
        assert!(
            !msg.contains("新快捷键"),
            "这条路径上没有新键，文案不该提它，实际：{msg}"
        );
    }

    #[test]
    fn the_unregister_message_says_both_keys_when_replacing() {
        // 换键那条路径：两个键都会唤起 → **两个都要说出来**。
        let msg = ShortcutError::UnregisterFailed {
            detail: "注销失败".to_string(),
            effective: Some("Cmd+K".to_string()),
            also_active: Some("Option+Cmd+Space".to_string()),
        }
        .message();
        assert!(
            msg.contains("Cmd+K") && msg.contains("Option+Cmd+Space"),
            "两个键都要说清，实际：{msg}"
        );
        assert!(
            msg.contains("同时生效"),
            "要说清两个都会唤起，实际：{msg}"
        );
    }

    // ---- 运行时真相：面板显示的快捷键全看这里 ----
    //
    // 【为什么要单独测】
    //   终审 I-1：启动时注册失败 → 设置里**一个字都没改** →
    //   面板显示设置里的值 → 面板说「⌥⌘Space 已生效」而用户按了没反应 = **骗人**。
    //   这三个纯函数就是「真相」的转移规则 —— 面板显示什么，全看它们。

    #[test]
    fn a_successful_apply_makes_the_requested_key_the_effective_one() {
        let rt = runtime_after_success(Some("Option+Cmd+Space"));
        assert_eq!(rt.effective.as_deref(), Some("Option+Cmd+Space"));
        assert_eq!(rt.error, None, "成功了就没有错误");
    }

    #[test]
    fn a_successful_apply_with_no_spec_means_no_key_is_effective() {
        // 「关掉快捷键」成功 → 一个键都不生效
        let rt = runtime_after_success(None);
        assert_eq!(rt.effective, None);
        assert_eq!(rt.error, None);
    }

    #[test]
    fn a_register_failure_changes_nothing_but_the_error() {
        // 【关键不变式】注册失败 = **什么都没变** ——
        //   生效的键还是原来那个（旧的键原封不动还在），
        //   只有「原因」变了。
        //   把 `effective` 也清掉 = 骗人说「现在一个键都不生效」，
        //   而旧键其实还在唤起 Lodestar。
        let prev = ShortcutRuntime {
            effective: Some("Option+Cmd+Space".to_string()),
            error: None,
        };
        let rt = runtime_after_failure(&prev, "快捷键「Cmd+K」注册失败".to_string());
        assert_eq!(
            rt.effective.as_deref(),
            Some("Option+Cmd+Space"),
            "注册失败不能动「生效的是哪个键」—— 旧键还在生效"
        );
        assert_eq!(
            rt.error.as_deref(),
            Some("快捷键「Cmd+K」注册失败"),
            "但原因必须记下来 —— 面板要能说清为什么没生效"
        );
    }

    #[test]
    fn an_unregister_failure_updates_the_effective_key_but_keeps_the_warning() {
        // 换键 B 成功、注销 A 失败 → 生效的是 **B**，但 A 可能还在 →
        // 面板必须两件事都说（否则用户以为 A 已经关掉了）。
        let rt = runtime_after_unregister_failure(
            Some("Cmd+K"),
            "旧快捷键「Option+Cmd+Space」注销失败".to_string(),
        );
        assert_eq!(rt.effective.as_deref(), Some("Cmd+K"));
        assert!(rt.error.is_some(), "旧键没注销掉这件事必须留在警告里");
    }
}
