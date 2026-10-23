//! 护栏 —— 层级令牌（`--z-*`）必须住在 `:root` 里，且 `z-index` **只能引用它们**。
//!
//! 【为什么要有这道护栏】
//! 「令牌化」最怕的事是**半年后又有人直接写 `z-index: 999`**。
//! 令牌化本身**不阻止**这件事 —— 任何人都能在任何一条规则里补一个裸数字，
//! 而那条裸数字会**静默**改掉叠放次序：编译不报错、运行不报错，
//! 只是出现「抽屉开着、预览卡却浮在上面」这种看得见但说不清的现象。
//! 这道护栏把它变成**红灯**。
//!
//! 【它钉住什么】
//!   Z1 `:root` 里定义了 `--z-titlebar` / `--z-popover` / `--z-menu` / `--z-modal`
//!      （且**恰好**这 4 个 —— 不多不少）
//!   Z2 `styles.css` 里**每一处** `z-index` 都必须是 `var(--z-*)`，
//!      而且**恰好**只有那 4 处（选择器与令牌都对得上）
//!   Z3 `--z-modal` > `--z-popover` 且 > `--z-menu`（**模态必须最高**）
//!   Z4 `--z-menu` > `--z-popover`（菜单压在预览卡上面）
//!   Z5 `--z-titlebar` < `--z-popover` 且 < `--z-menu`（标题栏在所有浮层之下）
//!
//! 【Z3/Z4/Z5 是本批次的「核心修复」】
//!   层级原本是**反的**：模态层（40）比两个小浮层（110/120）**低**。
//!   令牌表只是把这件事**说清楚**；真正防回退的是这三条**次序**断言 ——
//!   谁把 `--z-modal` 调到比 `--z-popover` 小，红灯立刻亮。
//!
//! 【为什么 Z2 用「枚举允许的 4 处」，而不是「扫有没有裸数字」】
//!   裸扫也能挡住 `z-index: 999`，但**更严**的是枚举：它同时钉住「有几处」
//!   和「在哪几个选择器上」。半年后有人加第 5 个浮层时：
//!     - 裸扫：写 `z-index: 50` → 红（对了）；写 `z-index: var(--z-modal)` → **假绿**
//!       （值合法，但层级语义被偷了 —— 新浮层跟模态同层）；
//!     - 枚举：两种写法**都红**（数量对不上）。
//!   所以枚举是「更严 + 更好读」的那一种。
//!   —— 但断言的是**语义事实**（令牌名 + 选择器名），
//!      **不是**「一个字符都不能差」的原文比对：换行、加空格、换顺序都不会误伤。
//!
//! 【为什么要先剥注释再断言】
//!   样式表里到处是解释性注释 —— 令牌表那段注释里就写着 `--z-titlebar` 这些名字，
//!   `.ctx-menu` 上方那句「比 .preview-card 高一层」也是注释。
//!   不剥注释就扫，会把**注释里的字样**当成真的声明 → 假红。
//!
//! 【为什么放 Rust 侧而不是 vitest】
//!   - `app/src-tauri/tests/` 已经有这个模式 —— `syntax_tables.rs` / `user_guide.rs`
//!     / `watch_contract.rs` 都是「读文件 → 断言契约」的护栏；
//!   - Rust 测试**不依赖 node/pnpm**：跑得快，也不会因为前端工具链坏掉而一起挂；
//!   - `cargo test` 是本项目的**硬验收项** —— 护栏跟着它跑，不会漏。
//!
//! 【判据 —— 每一条「怎么改会红」】
//!   ① `:root` 里删掉 `--z-modal`              → Z1 红
//!   ② 某个 `z-index: var(--z-*)` 改成裸 `999`  → Z2 红
//!   ③ `--z-modal` 调到比 `--z-popover` 小      → Z3 红
//!   ④ `--z-menu` 调到比 `--z-popover` 小       → Z4 红
//!   ⑤ `--z-titlebar` 调到比 `--z-popover` 大   → Z5 红

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// 层级令牌的名字 —— 【唯一真相】。
const Z_TOKENS: [&str; 4] = ["--z-titlebar", "--z-popover", "--z-menu", "--z-modal"];

/// 允许出现 `z-index` 的地方：选择器 → 令牌。
///
/// 【为什么是「枚举」而不是「扫有没有裸数字」】见文件开头的说明。
const ALLOWED_Z_SITES: [(&str, &str); 4] = [
    (".titlebar", "--z-titlebar"),
    (".preview-card", "--z-popover"),
    (".ctx-menu", "--z-menu"),
    (".panel-drawer-backdrop", "--z-modal"),
];

/// 样式表的绝对路径 —— 用 `CARGO_MANIFEST_DIR` 定位，测试从哪个目录跑都对。
///
/// （`CARGO_MANIFEST_DIR` = `app/src-tauri`，样式表在 `app/src/styles.css`。）
fn styles_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/styles.css")
}

/// 读样式表 —— 读不到就直接失败，并说清路径。
fn read_styles() -> (String, PathBuf) {
    let path = styles_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "读不到样式表 {}：{e}\n\
             （路径写错了？样式表应该在 app/src/styles.css。）",
            path.display()
        )
    });
    (text, path)
}

/// 剥掉 CSS 注释（`/* … */`）。
///
/// 【为什么要先剥注释】见文件开头的说明 —— 注释里写着 `--z-titlebar`
/// 这些名字，不剥就会被当成真的声明。
///
/// 注释整体换成**一个空格**（不是空串）—— 免得注释前后的两个 token 被粘在一起
/// （比如 `a/* x */b` 变成 `ab`）。
fn strip_comments(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            // 找 `*/`；找不到就吃掉到文件尾（不匹配的注释，交给浏览器去报错）
            let mut j = i + 2;
            while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                j += 1;
            }
            i = if j + 1 < chars.len() { j + 2 } else { chars.len() };
            out.push(' ');
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 一条声明：属性名 + 值 + 它所属的选择器。
struct Decl {
    /// 所属选择器（块栈顶）。
    selector: String,
    /// 属性名（如 `z-index` / `--z-modal`）。
    property: String,
    /// 值文本（已 trim）。
    value: String,
}

/// 结算当前缓冲区里的一条声明（`property: value`）。
///
/// 缓冲区里没有 `:` 就不是声明（忽略）—— 比如残缺的规则。
fn flush_decl(stack: &[String], buf: &mut String, decls: &mut Vec<Decl>) {
    let chunk = buf.trim().to_string();
    buf.clear();
    if chunk.is_empty() {
        return;
    }
    let Some((prop, value)) = chunk.split_once(':') else {
        return;
    };
    let prop = prop.trim();
    if prop.is_empty() {
        return;
    }
    decls.push(Decl {
        selector: stack.last().cloned().unwrap_or_default(),
        property: prop.to_string(),
        value: value.trim().to_string(),
    });
}

/// 扫出样式表里的所有声明（附上所属选择器）。
///
/// 【为什么自己扫而不用正则】
///   「这条声明属于哪个选择器」要看**块结构** —— 正则看不见花括号。
///   这里用一次单遍扫描维护**块栈**：`{` 进栈、`}` 出栈，
///   声明在 `;`（或块尾）结算，归属 = 栈顶的选择器。
///   引号里的内容整体跳过 —— `content: "a;b"` 不能被 `;` 切开。
fn parse_decls(src: &str) -> Vec<Decl> {
    let chars: Vec<char> = src.chars().collect();
    let mut stack: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut decls: Vec<Decl> = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                // 缓冲区里的是选择器（`:root` / `.titlebar` / `@media …`）。
                stack.push(buf.trim().to_string());
                buf.clear();
                i += 1;
            }
            '}' => {
                // 块尾 —— 最后一条声明可能没有分号，在这里结算。
                flush_decl(&stack, &mut buf, &mut decls);
                stack.pop();
                i += 1;
            }
            ';' => {
                flush_decl(&stack, &mut buf, &mut decls);
                i += 1;
            }
            '"' | '\'' => {
                // 引号串整体跳过（里面的 `;` / `{` / `}` 不是结构）。
                let quote = c;
                buf.push(c);
                i += 1;
                while i < chars.len() {
                    buf.push(chars[i]);
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        // 转义：下一个字符原样收下
                        buf.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if chars[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    decls
}

/// 选择器拆成「简单选择器」列表 —— `.app .titlebar` → `[".app", ".titlebar"]`。
///
/// 【为什么要拆】这样「`.titlebar` 上有一处 z-index」这种断言
/// 不会因为选择器写成 `.app .titlebar` 就误伤，也不会因为
/// `.titlebar-foo` 里含 `.titlebar` 子串就假绿。
fn selector_parts(selector: &str) -> Vec<String> {
    selector
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// 去掉值里的所有空白 —— `var( --z-modal )` 与 `var(--z-modal)` 视为同一个。
fn normalize(value: &str) -> String {
    value.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 从样式表里解析出来的**事实**。
struct Facts {
    /// `:root` 里的层级令牌（名字 → 数值）。
    tokens: BTreeMap<String, i64>,
    /// 全部 `z-index` 声明（选择器 + 值文本）。
    z_index: Vec<(String, String)>,
}

/// 解析样式表 —— **只做一次**（五个断言共用）。
///
/// 【为什么缓存】读文件 + 扫一遍不贵，但五个断言各扫一次会让人
/// 以为「一条规则被算了五遍」；缓存也让断言之间的观察保持一致。
fn facts() -> &'static Facts {
    static F: OnceLock<Facts> = OnceLock::new();
    F.get_or_init(|| {
        let (raw, _path) = read_styles();
        let styles = strip_comments(&raw);
        let decls = parse_decls(&styles);

        let mut tokens: BTreeMap<String, i64> = BTreeMap::new();
        let mut z_index: Vec<(String, String)> = Vec::new();

        for d in &decls {
            let in_root = selector_parts(&d.selector).iter().any(|p| p == ":root");

            if in_root && d.property.starts_with("--z-") {
                let v: i64 = d.value.trim().parse().unwrap_or_else(|_| {
                    panic!(
                        ":root 里的层级令牌 {} 的值不是整数：{:?}\n\
                         （层级令牌必须是**裸数字** —— var() 套 var() 会让浏览器算不出来。）",
                        d.property, d.value
                    )
                });
                tokens.insert(d.property.clone(), v);
            }

            if d.property == "z-index" {
                z_index.push((d.selector.clone(), d.value.clone()));
            }
        }

        Facts { tokens, z_index }
    })
}

/// 取一个令牌的值 —— 缺了就红（Z1 会先报，这里只是别 panic）。
fn token(name: &str) -> i64 {
    *facts()
        .tokens
        .get(name)
        .unwrap_or_else(|| panic!(":root 里没有定义层级令牌 {name} —— 见 Z1"))
}

// ---------------------------------------------------------------------------
// Z1 —— 四个令牌都在 :root 里，且不多不少
// ---------------------------------------------------------------------------

#[test]
fn z1_root_defines_exactly_the_four_level_tokens() {
    let f = facts();

    for name in Z_TOKENS {
        assert!(
            f.tokens.contains_key(name),
            ":root 里没有定义层级令牌 {name}。\n\
             （层级令牌必须住在 :root 里 —— 散落的魔法数字正是这次要消灭的东西。）"
        );
    }

    assert_eq!(
        f.tokens.len(),
        Z_TOKENS.len(),
        ":root 里的层级令牌有 {} 个（{}），但契约是**恰好** {} 个（{}）。\n\
         （多出来的令牌说明层级表走偏了 —— 要加令牌的话，先想清楚它属于哪一层，\
           再把本护栏的 Z_TOKENS / ALLOWED_Z_SITES 一起更新。）",
        f.tokens.len(),
        f.tokens.keys().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
        Z_TOKENS.len(),
        Z_TOKENS.join(", "),
    );
}

// ---------------------------------------------------------------------------
// Z2 —— 每一处 z-index 都是 var(--z-*)，且恰好只有那 4 处
// ---------------------------------------------------------------------------

#[test]
fn z2_every_z_index_uses_a_token_and_only_the_four_allowed_sites() {
    let f = facts();

    // (a) 不多不少 —— 恰好 4 处
    assert_eq!(
        f.z_index.len(),
        ALLOWED_Z_SITES.len(),
        "样式表里有 {} 处 `z-index`，契约是**恰好** {} 处。\n\
         实际有：{:?}。\n\
         （多一处 = 有人偷偷加了一层；少一处 = 层级令牌化被回退了。）",
        f.z_index.len(),
        ALLOWED_Z_SITES.len(),
        f.z_index
    );

    // (b) 每处的值都必须是 var(--z-*) —— 裸数字（999 这种）一律红
    for (sel, val) in &f.z_index {
        let norm = normalize(val);
        assert!(
            Z_TOKENS.iter().any(|t| norm == format!("var({t})")),
            "选择器 {sel} 上的 `z-index: {val}` 不是层级令牌的引用。\n\
             （只允许 `z-index: var(--z-*)` —— 裸数字是这次要消灭的魔法数字，\
               而且裸数字会**静默**改掉叠放次序。）"
        );
    }

    // (c) 每处都落在允许的选择器上，且令牌对得上
    for (want_sel, want_tok) in ALLOWED_Z_SITES {
        let hits: Vec<&(String, String)> = f
            .z_index
            .iter()
            .filter(|(sel, _)| selector_parts(sel).iter().any(|p| p == want_sel))
            .collect();

        assert_eq!(
            hits.len(),
            1,
            "选择器 {want_sel} 上应当【恰好】有一处 `z-index`，实际有 {} 处：{:?}。\n\
             （层级表只认这 4 个落点：{}）",
            hits.len(),
            hits,
            ALLOWED_Z_SITES
                .iter()
                .map(|(s, t)| format!("{s} → var({t})"))
                .collect::<Vec<_>>()
                .join("、")
        );

        let norm = normalize(&hits[0].1);
        assert_eq!(
            norm,
            format!("var({want_tok})"),
            "选择器 {want_sel} 上的 `z-index` 应当是 var({want_tok})，实际是 {}。",
            hits[0].1
        );
    }
}

// ---------------------------------------------------------------------------
// Z3 —— 模态必须最高（本批次的核心修复）
// ---------------------------------------------------------------------------

#[test]
fn z3_modal_must_be_above_every_floating_layer() {
    let modal = token("--z-modal");
    let popover = token("--z-popover");
    let menu = token("--z-menu");

    assert!(
        modal > popover,
        "--z-modal（{modal}）必须高于 --z-popover（{popover}）。\n\
         （模态是「盖住一切」的层；浮层是「飘在内容上」的小东西。\
           模态一旦比浮层低，就会出现「抽屉开着、预览卡却浮在上面」—— \
           层级**反了**正是这次要修的缺陷。）"
    );

    assert!(
        modal > menu,
        "--z-modal（{modal}）必须高于 --z-menu（{menu}）。\n\
         （模态必须高于**任何**浮层 —— 包括右键菜单。）"
    );
}

// ---------------------------------------------------------------------------
// Z4 —— 菜单压在预览卡上面
// ---------------------------------------------------------------------------

#[test]
fn z4_menu_must_be_above_the_popover() {
    let menu = token("--z-menu");
    let popover = token("--z-popover");

    assert!(
        menu > popover,
        "--z-menu（{menu}）必须高于 --z-popover（{popover}）。\n\
         （右键菜单是**最后一层** —— 菜单永远压在别的东西上面。）"
    );
}

// ---------------------------------------------------------------------------
// Z5 —— 标题栏在所有浮层之下
// ---------------------------------------------------------------------------

#[test]
fn z5_titlebar_must_be_below_the_floating_layers() {
    let titlebar = token("--z-titlebar");
    let popover = token("--z-popover");
    let menu = token("--z-menu");

    assert!(
        titlebar < popover,
        "--z-titlebar（{titlebar}）必须低于 --z-popover（{popover}）。\n\
         （标题栏要在普通内容之上，但要在所有浮层之下。）"
    );

    assert!(
        titlebar < menu,
        "--z-titlebar（{titlebar}）必须低于 --z-menu（{menu}）。\n\
         （标题栏要在普通内容之上，但要在所有浮层之下。）"
    );
}
