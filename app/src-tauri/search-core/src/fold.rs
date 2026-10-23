//! Unicode 折叠 —— 本引擎**最重要的一个提速点**。
//!
//! 【为什么要在【索引期】折叠，而不是【查询期】】
//!   上游引擎每次查询都对**每一个条目**做一次 Unicode 折叠
//!   （`ucasemap_utf8FoldCase` + `unorm2_getNFDInstance`）。
//!   实测差距：同样的引擎、同样的匹配逻辑，
//!     合成索引（名字都是 ASCII）查询 100,002 项 → **0.99ms**
//!     真实 `$HOME`（名字有中文、有组合字符）查询 8,352 项 → **2.6ms**
//!   折算成每条：合成 10ns/条 vs 真实 310ns/条 —— **差 31 倍**。
//!   差的那部分就是「每个条目每次查询都要重新折叠一遍」。
//!
//!   本引擎的做法：**索引时折一次**，把结果存进 `folded_name` / `folded_path`；
//!   查询时只折**查询串**（几十纳秒），然后直接字节比较。
//!   → 每条成本从「O(名字长度) 的折叠 + 分配」降到「一次 memcmp」。
//!
//! 【折叠的定义】NFD 归一化 → 转小写 → 再 NFD。
//!   - **NFD**（不是 NFC）：把「é」拆成「e + 组合尖音符」。
//!     macOS 的 HFS+ 历史上用 NFD，APFS 不做归一化 ——
//!     同一份文件在磁盘上可能是两种形态。用 NFD 折叠能让两种都匹配上。
//!   - 转小写：Rust 的 `str::to_lowercase()` 是 Unicode 感知的。
//!     （严格说「大小写折叠」与「转小写」在少数字符上不同 —— 比如 ß ——
//!     对「找文件」这个场景，转小写足够，且行为可预期。）
//!   - 折完再 NFD 一次：转小写可能产生非归一化的结果（罕见）。

use unicode_normalization::UnicodeNormalization;

/// NFD + 小写 + NFD。
pub fn fold(s: &str) -> String {
    let lowered: String = s.nfd().collect::<String>().to_lowercase();
    lowered.nfd().collect()
}

/// 在**原串**里找出所有命中区间（字节偏移）。
///
/// 【为什么不能直接用折叠后的串的偏移】
///   折叠会改变长度（「İ」→ 两码点），折叠串的偏移套到原串上会错位。
///   → 这里在**原串**上按字符走一遍，逐字符折叠后比较。
///
/// 【复杂度】O(n·m)（n = 原串长度，m = needle 长度）。
///   文件名只有几十字节，而且**只对当前页的条目做**（不是全表），
///   所以完全可以接受。
///
/// 【返回】`(起始字节, 结束字节)` 列表，字节偏移，可直接切成 `Highlight`。
pub fn find_folded(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle_chars: Vec<char> = needle.chars().collect();

    // 原串 → [(字节起, 字节止, 折叠后的字符)]。
    // 一个原字符可能折成多个（如 İ → i + 组合符），它们共用同一对字节偏移 ——
    // 这样命中区间的起止永远落在**原字符边界**上，不会切出半个字符。
    let mut items: Vec<(usize, usize, char)> = Vec::with_capacity(hay.len());
    for (off, ch) in hay.char_indices() {
        let end = off + ch.len_utf8();
        for folded in fold(&ch.to_string()).chars() {
            items.push((off, end, folded));
        }
    }

    let n = needle_chars.len();
    if items.len() < n {
        return Vec::new();
    }

    let mut out = Vec::new();
    for i in 0..=(items.len() - n) {
        let hit = (0..n).all(|k| items[i + k].2 == needle_chars[k]);
        if hit {
            let start = items[i].0;
            let end = items[i + n - 1].1;
            out.push((start, end));
        }
    }
    out
}
