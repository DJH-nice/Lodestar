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
    // 【ASCII 快路径 —— 语义完全等价，不是「差不多」】
    //   ASCII 字符**没有** Unicode 分解（NFD 对纯 ASCII 是恒等变换），
    //   也不存在 Unicode 大小写特例 → 对纯 ASCII 串来说，
    //   「NFD → 转小写 → NFD」的结果与「ASCII 转小写」**逐字节相同**。
    //
    // 【为什么值得单开一条路】
    //   折叠的实测开销里 **92% 是 NFD**（探针实测：8,548 条目
    //   `fold(name)+fold(path)` 共 17.7ms，其中 NFD 净增 16.3ms），
    //   而绝大多数文件名是纯 ASCII → 这一条快路径能省掉绝大部分折叠开销。
    //
    // 【等价性的证据】
    //   单测 `ascii_fast_path_matches_the_general_path` 把两条路径
    //   在一批 ASCII 用例上**逐条对账** —— 谁改坏了都会红。
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let lowered: String = s.nfd().collect::<String>().to_lowercase();
    lowered.nfd().collect()
}

/// 在**原串**里找出所有命中区间（字节偏移）。
///
/// 【needle 必须是「已折叠」的串】
///   调用方传的是 `Pattern::Plain.folded`（用 [`fold`] 折过的查询串）。
///   **不是**原样的查询串 —— 本函数只折叠 haystack（逐字符），
///   不折叠 needle → 传原串进来，非 ASCII 的 needle 永远匹配不上。
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
        if ch.is_ascii() {
            // 【ASCII 快路径 —— 与通用路径完全等价】
            //   单个 ASCII 字符的折叠 = ASCII 转小写
            //   （ASCII 的 NFD 是恒等变换 → 通用路径的结果就是这个）。
            //   省掉每个字符一次 String 分配 + 一次完整 NFD ——
            //   一页 500 条 × 上百字符 = 十万次分配，全靠这条快路径消掉。
            items.push((off, end, ch.to_ascii_lowercase()));
        } else {
            for folded in fold(&ch.to_string()).chars() {
                items.push((off, end, folded));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_normalization::UnicodeNormalization;

    /// 通用路径（不含 ASCII 快路径）—— 用来跟快路径**对账**。
    fn fold_general(s: &str) -> String {
        let lowered: String = s.nfd().collect::<String>().to_lowercase();
        lowered.nfd().collect()
    }

    #[test]
    fn ascii_fast_path_matches_the_general_path() {
        // 【这条是 ASCII 快路径的护栏】
        //   快路径只是「换一条等价算法」，不是「近似」——
        //   任何一条用例对不上，都说明快路径改变了语义 → 立刻红。
        for s in [
            "",
            "a",
            "Z",
            "README.TXT",
            "MiXeD",
            "Foo Bar 123",
            "~",
            "a1_-.+",
            "  空格  ",
            "0",
        ] {
            assert_eq!(
                fold(s),
                fold_general(s),
                "ASCII 快路径与通用路径不一致：{s:?}"
            );
        }
    }

    #[test]
    fn ascii_is_lowercased_and_left_normalized() {
        assert_eq!(fold("README.TXT"), "readme.txt");
        assert_eq!(fold("MiXeD"), "mixed");
        assert_eq!(fold(""), "");
    }

    #[test]
    fn non_ascii_still_goes_through_nfd() {
        // 非 ASCII 必须继续走 NFD —— 「é」要拆成「e + 组合尖音符」
        assert_eq!(fold("ÉCLAIR.txt"), "e\u{301}clair.txt");
        assert_eq!(fold("报告.pdf"), "报告.pdf");
        // 德语 ß 转小写后仍是 ß（Rust 的 to_lowercase 不是 full case folding）
        assert_eq!(fold("STRASSE"), "strasse");
    }

    #[test]
    fn find_folded_reports_byte_offsets_on_the_original_string() {
        // 命中区间必须是**原串**的字节偏移
        let hay = "Report FINAL.txt";
        let hits = find_folded(hay, "final");
        assert_eq!(hits, vec![(7, 12)], "FINAL 的字节区间不对");

        // 大小写不敏感
        assert_eq!(find_folded(hay, "final"), vec![(7, 12)]);
        assert_eq!(find_folded(hay, "report"), vec![(0, 6)]);
        // 找不到
        assert!(find_folded(hay, "zzz").is_empty());
    }

    #[test]
    fn find_folded_handles_non_ascii() {
        // 「é」在原串里可能是 1 个字符或 2 个字符，
        // 命中区间必须落在**原字符边界**上，不能切出半个字符
        let hay = "café.txt";
        // ⚠️ needle 必须是**已折叠**的（调用方传的是 Pattern::Plain.folded）
        let hits = find_folded(hay, &fold("café"));
        assert_eq!(hits.len(), 1, "非 ASCII 的 needle 也要能命中");
        let (start, end) = hits[0];
        assert_eq!(&hay[start..end], "café");
    }

    #[test]
    fn the_ascii_fast_path_in_find_folded_matches_the_general_path() {
        // 【ASCII 快路径的护栏】
        //   快路径逐字符折叠，结果必须与「每个字符都走通用折叠」**逐条相同**。
        fn general(hay: &str, needle: &str) -> Vec<(usize, usize)> {
            let needle_chars: Vec<char> = needle.chars().collect();
            let mut items: Vec<(usize, usize, char)> = Vec::new();
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
                if (0..n).all(|k| items[i + k].2 == needle_chars[k]) {
                    out.push((items[i].0, items[i + n - 1].1));
                }
            }
            out
        }
        for (hay, needle) in [
            ("Report FINAL.txt", "final"),
            ("README.TXT", "readme"),
            ("MiXeD", "mixed"),
            ("a-b_c.d", "_c"),
            ("no match here", "zzz"),
        ] {
            let folded_needle = fold(needle);
            assert_eq!(
                find_folded(hay, &folded_needle),
                general(hay, &folded_needle),
                "ASCII 快路径与通用路径不一致：{hay:?} / {needle:?}"
            );
        }
    }

    #[test]
    fn find_folded_needs_a_folded_needle() {
        // 钉住契约：needle 不折叠 → 非 ASCII 的 needle 匹配不上。
        // （这是**文档化的契约**，不是 bug —— 调用方传的是 folded needle。）
        let hay = "café.txt";
        assert!(
            find_folded(hay, "café").is_empty(),
            "未折叠的 needle 不该命中（契约：needle 必须已折叠）"
        );
    }
}
