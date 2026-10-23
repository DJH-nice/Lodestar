//! 查询引擎 —— 把用户敲的一串字变成「哪些条目命中」。
//!
//! 【设计原则：自己写，不抄上游】
//!   上游的 parser 是 GPL 代码，不能抄。这里是**独立实现**：
//!   能力对齐（字段、布尔、通配、正则、smart case、路径搜索），
//!   但**代码与结构都是自己的**。
//!
//! 【语法（我们这一版）】
//!   - 空格分隔；相邻词之间**隐式 AND**
//!   - `AND` / `&&` / `OR` / `||` / `NOT` / `!` + 括号
//!   - `字段:值`（`ext:pdf` `size:>10M` `dm:today` `path:Documents` `depth:1`）
//!   - `字段:运算符值`（`size:>10M` `dm:>2026-01-01`）
//!   - `dm:A..B` 区间
//!   - 修饰符：`file` `folder` `exact` `case` `nocase` `path` `nopath` `regex`
//!     裸用是开关（`file` = 只看文件），带冒号是「用这个方式匹配后面的词」
//!     （`file:报告` = 只在文件里找「报告」）
//!   - 引号短语：`"my file.txt"`
//!   - 通配：`*` `?`
//!   - **smart case**：词里有大写字母 → 区分大小写；否则不区分
//!
//! 【为什么要「索引期折叠」】
//!   见 `fold.rs`：匹配时只折**查询串**一次，条目侧是预先折好的。

use std::collections::HashSet;

use crate::fold::{find_folded, fold};
use crate::syntax;
use crate::types::IndexEntry;

/// 比较运算符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

/// 匹配目标：文件名还是完整路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Name,
    Path,
}

/// 匹配模式。
#[derive(Debug, Clone)]
pub enum Pattern {
    /// 普通子串（带折叠后的版本）。
    Plain { needle: String, folded: String },
    /// 正则（大小写敏感性在构造时已定）。
    Regex(Box<regex::Regex>),
}

impl Pattern {
    /// 在文本里找出所有命中区间（字节偏移）。
    fn ranges(&self, text: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
        match self {
            Pattern::Plain { needle, folded } => {
                if case_sensitive {
                    text.match_indices(needle.as_str())
                        .map(|(i, _)| (i, i + needle.len()))
                        .collect()
                } else {
                    find_folded(text, folded)
                }
            }
            Pattern::Regex(re) => re
                .find_iter(text)
                .map(|m| (m.start(), m.end()))
                .collect(),
        }
    }
}

/// 一个文本条件。
#[derive(Debug, Clone)]
pub struct TextTerm {
    pub pattern: Pattern,
    pub target: Target,
    pub case_sensitive: bool,
    /// 整串相等（`exact:`）。
    pub exact: bool,
}

/// 字段条件。
#[derive(Debug, Clone)]
pub enum FieldTerm {
    /// `ext:pdf` / `ext:pdf;doc`
    Ext { values: Vec<String> },
    /// `size:>10M`
    Size { op: CmpOp, bytes: i64 },
    /// `dm:today` / `dm:2026-10-01..2026-10-23`
    Mtime { from: i64, to: i64 },
    /// `depth:1`
    Depth { op: CmpOp, value: u32 },
    /// `empty:` —— 空文件或空目录
    Empty,
    /// `childcount:1`
    ChildCount { op: CmpOp, value: u32 },
    /// `parent:/Users/me/Downloads` —— 父目录路径相等
    Parent { needle: String },
    /// `parents:Documents` —— 任一祖先路径含这一段
    Parents { needle: String },
    /// `pic:` / `audio:` / `video:` / `doc:` / `archive:` / `app:` / `contenttype:`
    Group { name: String },
}

/// 语法树节点。
#[derive(Debug, Clone)]
pub enum Node {
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Not(Box<Node>),
    Text(TextTerm),
    Field(FieldTerm),
    /// 只看文件 / 只看目录
    Kind { folders: bool },
}

/// 解析好的查询。
#[derive(Debug, Clone)]
pub struct Query {
    pub root: Node,
    /// 只看文件 / 只看目录（查询级开关）。
    pub only_files: bool,
    pub only_folders: bool,
    /// 警告（未知字段、暂未实现的字段…）—— 给状态栏显示。
    pub warnings: Vec<String>,
}

impl Query {
    /// 这条查询要不要看这个条目。
    pub fn matches(&self, e: &IndexEntry) -> bool {
        if self.only_files && e.is_folder {
            return false;
        }
        if self.only_folders && !e.is_folder {
            return false;
        }
        eval(&self.root, e)
    }
}

fn eval(node: &Node, e: &IndexEntry) -> bool {
    match node {
        Node::And(a, b) => eval(a, e) && eval(b, e),
        Node::Or(a, b) => eval(a, e) || eval(b, e),
        Node::Not(a) => !eval(a, e),
        Node::Text(t) => match_text(t, e),
        Node::Field(f) => match_field(f, e),
        Node::Kind { folders } => e.is_folder == *folders,
    }
}

fn match_text(t: &TextTerm, e: &IndexEntry) -> bool {
    let (text, folded_text) = match t.target {
        Target::Name => (e.name.as_str(), e.folded_name.as_str()),
        Target::Path => (e.path.as_str(), e.folded_path.as_str()),
    };
    match &t.pattern {
        Pattern::Plain { needle, folded } => {
            if t.exact {
                if t.case_sensitive {
                    text == needle
                } else {
                    folded_text == folded
                }
            } else if t.case_sensitive {
                text.contains(needle.as_str())
            } else {
                folded_text.contains(folded.as_str())
            }
        }
        Pattern::Regex(re) => re.is_match(text),
    }
}

fn match_field(f: &FieldTerm, e: &IndexEntry) -> bool {
    match f {
        FieldTerm::Ext { values } => {
            if values.is_empty() {
                // `ext:` 空值 = 「有扩展名的条目」
                !e.extension.is_empty()
            } else {
                values.iter().any(|v| v == &e.extension)
            }
        }
        FieldTerm::Size { op, bytes } => cmp_i64(e.size, *op, *bytes),
        FieldTerm::Mtime { from, to } => e.mtime >= *from && e.mtime <= *to,
        FieldTerm::Depth { op, value } => cmp_u32(e.depth, *op, *value),
        FieldTerm::Empty => {
            if e.is_folder {
                e.child_count == 0
            } else {
                e.size == 0
            }
        }
        FieldTerm::ChildCount { op, value } => cmp_u32(e.child_count, *op, *value),
        FieldTerm::Parent { needle } => fold(&e.dir) == *needle,
        FieldTerm::Parents { needle } => {
            // 任一祖先目录的路径含这一段
            let mut cur = std::path::Path::new(&e.path);
            while let Some(p) = cur.parent() {
                if fold(&p.to_string_lossy()).contains(needle.as_str()) {
                    return true;
                }
                cur = p;
            }
            false
        }
        FieldTerm::Group { name } => group_matches(name, e),
    }
}

fn cmp_i64(a: i64, op: CmpOp, b: i64) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
    }
}

fn cmp_u32(a: u32, op: CmpOp, b: u32) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Gt => a > b,
        CmpOp::Ge => a >= b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
    }
}

/// 扩展名分组（我们自己定的表 —— 不是上游的）。
fn group_matches(name: &str, e: &IndexEntry) -> bool {
    let ext = e.extension.as_str();
    let table: &[&str] = match name {
        "pic" | "image" => &["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp", "heic", "heif", "svg", "ico"],
        "audio" => &["mp3", "wav", "aac", "flac", "m4a", "ogg", "aiff", "alac"],
        "video" => &["mp4", "mov", "avi", "mkv", "webm", "m4v", "mpg", "mpeg", "wmv"],
        "archive" => &["zip", "tar", "gz", "bz2", "xz", "7z", "rar", "tgz", "zst"],
        "doc" => &["pdf", "doc", "docx", "rtf", "odt", "pages", "txt", "md", "markdown", "ppt", "pptx", "xls", "xlsx", "csv"],
        "app" => &["app", "bundle", "exe", "msi", "pkg", "dmg"],
        "text" => &["txt", "md", "markdown", "rtf", "csv", "json", "xml", "yaml", "yml", "toml", "log", "html", "htm", "ts", "tsx", "js", "jsx", "rs", "py", "go", "java", "c", "h", "cpp", "hpp", "sh", "swift", "kt"],
        _ => &[],
    };
    table.contains(&ext)
}

// ---------------------------------------------------------------- 词法

/// 词法单元。
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 普通词（可能是字段名、修饰符名、关键字）
    Word(String),
    /// 引号里的短语
    Quoted(String),
    /// 冒号（字段分隔）
    Colon,
    /// 运算符 / 关键字
    Op(String),
    LParen,
    RParen,
}

fn lex(input: &str) -> Vec<Tok> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }

        // 引号短语
        if c == '"' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '"' {
                j += 1;
            }
            out.push(Tok::Quoted(chars[start..j].iter().collect()));
            i = if j < chars.len() { j + 1 } else { j };
            continue;
        }

        // 括号
        if c == '(' {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }

        // 冒号
        if c == ':' {
            out.push(Tok::Colon);
            i += 1;
            continue;
        }

        // 运算符（单字符与双字符）
        if matches!(c, '<' | '>' | '=' | '!' | '&' | '|') {
            let mut op = String::new();
            op.push(c);
            i += 1;
            if i < chars.len() && chars[i] == c && matches!(c, '<' | '>' | '=' | '&' | '|') {
                op.push(c);
                i += 1;
            }
            out.push(Tok::Op(op));
            continue;
        }

        // 普通词：吃到下一个空白 / 括号 / 冒号 / 运算符为止
        let start = i;
        while i < chars.len() {
            let ch = chars[i];
            if ch.is_whitespace()
                || matches!(ch, '(' | ')' | ':' | '<' | '>' | '=' | '!' | '&' | '|')
            {
                break;
            }
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        // 关键字（大小写不敏感）
        let upper = word.to_uppercase();
        if matches!(upper.as_str(), "AND" | "OR" | "NOT") {
            out.push(Tok::Op(upper));
        } else {
            out.push(Tok::Word(word));
        }
    }
    out
}

// ---------------------------------------------------------------- 语法

/// 待生效的修饰符（裸修饰符作用于**下一个**词）。
#[derive(Debug, Clone, Default)]
struct Pending {
    case_sensitive: Option<bool>,
    exact: bool,
    regex: bool,
    target: Option<Target>,
}

/// 解析器。
struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    pending: Pending,
    warnings: Vec<String>,
    only_files: bool,
    only_folders: bool,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Option<Node> {
        let mut left = self.parse_and()?;
        loop {
            match self.peek() {
                Some(Tok::Op(op)) if op == "OR" || op == "||" => {
                    self.next();
                    let right = self.parse_and()?;
                    left = Node::Or(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Some(left)
    }

    fn parse_and(&mut self) -> Option<Node> {
        let mut left = self.parse_not()?;
        loop {
            // 隐式 AND：下一个 token 能开始一个 term 就是 AND
            match self.peek() {
                Some(Tok::Op(op)) if op == "AND" || op == "&&" => {
                    self.next();
                    let right = self.parse_not()?;
                    left = Node::And(Box::new(left), Box::new(right));
                }
                Some(Tok::Word(_)) | Some(Tok::Quoted(_)) | Some(Tok::LParen) => {
                    let right = self.parse_not()?;
                    left = Node::And(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Some(left)
    }

    fn parse_not(&mut self) -> Option<Node> {
        match self.peek() {
            Some(Tok::Op(op)) if op == "NOT" || op == "!" => {
                self.next();
                let inner = self.parse_not()?;
                Some(Node::Not(Box::new(inner)))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Option<Node> {
        match self.next() {
            Some(Tok::LParen) => {
                let inner = self.parse_or()?;
                // 吃掉右括号（没有也不报错 —— 用户打到一半是很常见的）
                if matches!(self.peek(), Some(Tok::RParen)) {
                    self.next();
                }
                Some(inner)
            }
            Some(Tok::Word(w)) => self.parse_word(w),
            Some(Tok::Quoted(q)) => Some(self.make_text(q, false)),
            _ => None,
        }
    }

    /// 处理一个普通词 —— 可能是字段、修饰符或文本。
    fn parse_word(&mut self, word: String) -> Option<Node> {
        let lower = word.to_lowercase();

        // 字段 / 修饰符后跟冒号？
        if matches!(self.peek(), Some(Tok::Colon)) {
            self.next(); // 吃掉 ':'
            // 值可能是运算符 + 词，或直接是词/引号
            let (op, value, quoted) = self.read_value();
            return Some(self.make_field_or_modifier(lower, op, value, quoted));
        }

        // 裸修饰符
        if syntax::modifier_supported(&lower) {
            match lower.as_str() {
                "file" | "files" => {
                    self.only_files = true;
                    return Some(Node::Kind { folders: false });
                }
                "folder" | "folders" => {
                    self.only_folders = true;
                    return Some(Node::Kind { folders: true });
                }
                "case" => self.pending.case_sensitive = Some(true),
                "nocase" => self.pending.case_sensitive = Some(false),
                "exact" => self.pending.exact = true,
                "regex" => self.pending.regex = true,
                "path" => self.pending.target = Some(Target::Path),
                "nopath" => self.pending.target = Some(Target::Name),
                _ => {}
            }
            // 裸修饰符本身不产生节点 —— 它作用于下一个词。
            // 但如果后面没有词了，它就只是个开关（已经被上面的分支处理）。
            return self.parse_not();
        }

        Some(self.make_text(word, false))
    }

    /// 读 `字段:` 后面的值。
    fn read_value(&mut self) -> (CmpOp, String, bool) {
        let mut op = CmpOp::Eq;
        if let Some(Tok::Op(o)) = self.peek().cloned() {
            match o.as_str() {
                ">" => {
                    op = CmpOp::Gt;
                    self.next();
                }
                ">=" => {
                    op = CmpOp::Ge;
                    self.next();
                }
                "<" => {
                    op = CmpOp::Lt;
                    self.next();
                }
                "<=" => {
                    op = CmpOp::Le;
                    self.next();
                }
                "=" => {
                    op = CmpOp::Eq;
                    self.next();
                }
                "!=" => {
                    op = CmpOp::Eq; // 取反由字段自己处理
                    self.next();
                }
                _ => {}
            }
        }
        match self.next() {
            Some(Tok::Word(w)) => (op, w, false),
            Some(Tok::Quoted(q)) => (op, q, true),
            _ => (op, String::new(), false),
        }
    }

    fn make_field_or_modifier(
        &mut self,
        name: String,
        op: CmpOp,
        value: String,
        quoted: bool,
    ) -> Node {
        // 修饰符（带值）
        if syntax::modifier_supported(&name) {
            let mut term = self.make_text(value.clone(), quoted);
            if let Node::Text(t) = &mut term {
                match name.as_str() {
                    "exact" => t.exact = true,
                    "case" => t.case_sensitive = true,
                    "nocase" => t.case_sensitive = false,
                    "path" => t.target = Target::Path,
                    "nopath" => t.target = Target::Name,
                    "regex" => {
                        if let Pattern::Plain { .. } = &t.pattern {
                            t.pattern = build_regex(&value, false, true);
                        }
                    }
                    "noregex" => {}
                    _ => {}
                }
                // `file:报告` / `folder:Downloads` 还要限定类型
                if name == "file" || name == "files" {
                    self.only_files = true;
                } else if name == "folder" || name == "folders" {
                    self.only_folders = true;
                }
            }
            return term;
        }

        // 字段
        if !syntax::FIELDS.contains(&name.as_str()) {
            self.warnings.push(format!("未知字段：{name}"));
            return self.make_text(String::new(), false);
        }
        if !syntax::field_supported(&name) {
            self.warnings.push(format!("字段 {name} 暂未实现"));
            return self.make_text(String::new(), false);
        }

        let folded_value = fold(&value);
        let term = match name.as_str() {
            "ext" => {
                let values: Vec<String> = value
                    .split(';')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                FieldTerm::Ext { values }
            }
            "size" => match parse_size(&value) {
                Some(bytes) => FieldTerm::Size { op, bytes },
                None => {
                    self.warnings.push(format!("看不懂的大小：{value}"));
                    FieldTerm::Size {
                        op: CmpOp::Lt,
                        bytes: 0,
                    }
                }
            },
            "dm" | "datemodified" => match parse_date_range(&value) {
                Some((from, to)) => FieldTerm::Mtime { from, to },
                None => {
                    self.warnings.push(format!("看不懂的日期：{value}"));
                    FieldTerm::Mtime { from: 0, to: 0 }
                }
            },
            "depth" => match value.parse::<u32>() {
                Ok(v) => FieldTerm::Depth { op, value: v },
                Err(_) => {
                    self.warnings.push(format!("看不懂的深度：{value}"));
                    FieldTerm::Depth { op, value: 0 }
                }
            },
            "empty" => FieldTerm::Empty,
            "childcount" => match value.parse::<u32>() {
                Ok(v) => FieldTerm::ChildCount { op, value: v },
                Err(_) => {
                    self.warnings.push(format!("看不懂的子项数：{value}"));
                    FieldTerm::ChildCount { op, value: 0 }
                }
            },
            "parent" => FieldTerm::Parent { needle: folded_value },
            "parents" => FieldTerm::Parents { needle: folded_value },
            "contenttype" => FieldTerm::Group {
                name: value.to_lowercase(),
            },
            other => FieldTerm::Group {
                name: other.to_string(),
            },
        };
        Node::Field(term)
    }

    /// 造一个文本条件。
    fn make_text(&mut self, needle: String, quoted: bool) -> Node {
        let pending = std::mem::take(&mut self.pending);

        // smart case：词里有大写字母 → 区分大小写
        let has_upper = needle.chars().any(|c| c.is_uppercase());
        let case_sensitive = pending.case_sensitive.unwrap_or(has_upper);

        // 含 '/' 的词 → 搜路径（这是通用习惯，不是上游独创）
        let target = pending
            .target
            .unwrap_or_else(|| if needle.contains('/') && !quoted {
                Target::Path
            } else {
                Target::Name
            });

        let pattern = if pending.regex {
            build_regex(&needle, case_sensitive, false)
        } else if needle.contains('*') || needle.contains('?') {
            build_glob_regex(&needle, case_sensitive)
        } else {
            Pattern::Plain {
                folded: fold(&needle),
                needle,
            }
        };

        Node::Text(TextTerm {
            pattern,
            target,
            case_sensitive,
            exact: pending.exact,
        })
    }
}

/// 把通配符模式转成正则（**整串匹配**）。
fn build_glob_regex(pattern: &str, case_sensitive: bool) -> Pattern {
    let mut re = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            c => {
                re.push_str(&regex::escape(&c.to_string()));
            }
        }
    }
    re.push('$');
    Pattern::Regex(Box::new(
        regex::RegexBuilder::new(&re)
            .case_insensitive(!case_sensitive)
            .build()
            .expect("通配符转正则一定合法"),
    ))
}

/// 把 `/.../` 形式的正则字面量转成正则。
///
/// 【为什么正则整体要匹配】
///   用户写 `regex:"^gam"` 是想匹配**开头** —— 整体匹配会让 `^` 失去意义。
///   → 正则用「搜索」语义（`is_match` / `find_iter`），不是整体匹配。
fn build_regex(pattern: &str, case_sensitive: bool, _quoted: bool) -> Pattern {
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build();
    match re {
        Ok(r) => Pattern::Regex(Box::new(r)),
        Err(e) => {
            // 非法正则 → 退化成普通子串（总比整个查询失败强）
            eprintln!("[sextant] 正则编译失败（{e}），退化成普通匹配：{pattern}");
            Pattern::Plain {
                folded: fold(pattern),
                needle: pattern.to_string(),
            }
        }
    }
}

/// 解析大小：`10M` `1.5M` `500` `2GB`。
///
/// 【单位是十进制】`1M = 1_000_000`（不是 1_048_576）——
///   速查卡里明写了这一点（上游也是十进制）。
fn parse_size(s: &str) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (num_part, unit_part) = split_size(t);
    let value: f64 = num_part.parse().ok()?;
    let mult: f64 = match unit_part.to_uppercase().as_str() {
        "" | "B" => 1.0,
        "K" | "KB" => 1e3,
        "M" | "MB" => 1e6,
        "G" | "GB" => 1e9,
        "T" | "TB" => 1e12,
        _ => return None,
    };
    Some((value * mult) as i64)
}

fn split_size(s: &str) -> (&str, &str) {
    let idx = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    (&s[..idx], s[idx..].trim())
}

// ---------------------------------------------------------------- 日期

/// 解析日期表达式 → `(from, to)`（闭区间，秒）。
pub fn parse_date_range(s: &str) -> Option<(i64, i64)> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    // 区间：A..B
    if let Some((a, b)) = t.split_once("..") {
        let from = parse_date_point(a.trim(), false)?;
        let to = parse_date_point(b.trim(), true)?;
        return Some((from, to));
    }
    // 比较：>DATE / <DATE / >=DATE / <=DATE / =DATE
    if let Some(rest) = t.strip_prefix(">=") {
        let p = parse_date_point(rest.trim(), false)?;
        return Some((p, i64::MAX));
    }
    if let Some(rest) = t.strip_prefix("<=") {
        let p = parse_date_point(rest.trim(), true)?;
        return Some((i64::MIN, p));
    }
    if let Some(rest) = t.strip_prefix('>') {
        let p = parse_date_point(rest.trim(), false)?;
        return Some((p + 1, i64::MAX));
    }
    if let Some(rest) = t.strip_prefix('<') {
        let p = parse_date_point(rest.trim(), true)?;
        return Some((i64::MIN, p - 1));
    }
    let from = parse_date_point(&t, false)?;
    let to = parse_date_point(&t, true)?;
    Some((from, to))
}

/// 解析一个日期点 → 起始秒（`end_of_unit=true` 时取该单位的**末尾**）。
fn parse_date_point(s: &str, end_of_unit: bool) -> Option<i64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let now = now_epoch();
    let (y, m, d) = today_civil(now);

    match t {
        "today" => {
            let start = start_of_day(y, m, d, now);
            Some(if end_of_unit { start + 86400 - 1 } else { start })
        }
        "yesterday" => {
            let (py, pm, pd) = civil_from_days(days_from_civil(y, m, d) - 1);
            let start = start_of_day(py, pm, pd, now);
            Some(if end_of_unit { start + 86400 - 1 } else { start })
        }
        "thisyear" => {
            let start = start_of_day(y, 1, 1, now);
            Some(if end_of_unit { start_of_day(y + 1, 1, 1, now) - 1 } else { start })
        }
        "lastyear" => {
            let start = start_of_day(y - 1, 1, 1, now);
            Some(if end_of_unit { start_of_day(y, 1, 1, now) - 1 } else { start })
        }
        "thismonth" => {
            let start = start_of_day(y, m, 1, now);
            let (ny, nm) = next_month(y, m);
            Some(if end_of_unit {
                start_of_day(ny, nm, 1, now) - 1
            } else {
                start
            })
        }
        "lastmonth" => {
            let (py, pm) = prev_month(y, m);
            let start = start_of_day(py, pm, 1, now);
            Some(if end_of_unit { start_of_day(y, m, 1, now) - 1 } else { start })
        }
        "mon" | "monday" => {
            // 本周一
            let days = days_from_civil(y, m, d);
            let dow = (days + 4).rem_euclid(7); // 1970-01-01 是周四
            let back = dow as i64; // 周一=0
            let (py, pm, pd) = civil_from_days(days - back);
            let start = start_of_day(py, pm, pd, now);
            Some(if end_of_unit { start + 86400 - 1 } else { start })
        }
        _ => {
            // pastNdays / pastNweeks / pastNmonths（带数字）
            //
            // 【为什么不能在这里用 `?` 提前返回】
            //   `split_past` 在「没有数字」时返回 None ——
            //   用 `?` 会让整个函数直接返回 None，
            //   下面那个「不带数字的相对形式」分支就永远走不到。
            //   → 这里必须用 if let，失败时**继续往下试**。
            if let Some(rest) = t.strip_prefix("past") {
                if let Some((n, unit)) = split_past(rest) {
                    let days_back: i64 = match unit {
                        "day" | "days" => n,
                        "week" | "weeks" => n * 7,
                        "month" | "months" => n * 30,
                        "year" | "years" => n * 365,
                        _ => return None,
                    };
                    let start = start_of_day(y, m, d, now) - (days_back - 1) * 86400;
                    return Some(if end_of_unit { start + days_back * 86400 - 1 } else { start });
                }
                // 不带数字的相对形式：pastweek / pastmonth / pastyear / pastday
                //
                // 【为什么要单列】速查卡里写的是 `dm:pastmonth`（不是 `past1month`）——
                //   用户照着抄，得能认。
                let days_back: i64 = match rest {
                    "day" => 1,
                    "week" => 7,
                    "month" => 30,
                    "year" => 365,
                    _ => return None,
                };
                let start = start_of_day(y, m, d, now) - (days_back - 1) * 86400;
                return Some(if end_of_unit { start + days_back * 86400 - 1 } else { start });
            }
            // YYYY / YYYY-MM / YYYY-MM-DD
            let parts: Vec<&str> = t.split('-').collect();
            match parts.len() {
                1 => {
                    let yy: i64 = parts[0].parse().ok()?;
                    let start = start_of_day(yy, 1, 1, now);
                    Some(if end_of_unit { start_of_day(yy + 1, 1, 1, now) - 1 } else { start })
                }
                2 => {
                    let yy: i64 = parts[0].parse().ok()?;
                    let mm: i64 = parts[1].parse().ok()?;
                    let start = start_of_day(yy, mm, 1, now);
                    let (ny, nm) = next_month(yy, mm);
                    Some(if end_of_unit {
                        start_of_day(ny, nm, 1, now) - 1
                    } else {
                        start
                    })
                }
                3 => {
                    let yy: i64 = parts[0].parse().ok()?;
                    let mm: i64 = parts[1].parse().ok()?;
                    let dd: i64 = parts[2].parse().ok()?;
                    let start = start_of_day(yy, mm, dd, now);
                    Some(if end_of_unit { start + 86400 - 1 } else { start })
                }
                _ => None,
            }
        }
    }
}

/// 拆 `3days` → `(3, "days")`。
fn split_past(rest: &str) -> Option<(i64, &str)> {
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    let unit = rest[digits.len()..].trim();
    Some((n, unit))
}

fn next_month(y: i64, m: i64) -> (i64, i64) {
    if m == 12 {
        (y + 1, 1)
    } else {
        (y, m + 1)
    }
}

fn prev_month(y: i64, m: i64) -> (i64, i64) {
    if m == 1 {
        (y - 1, 12)
    } else {
        (y, m - 1)
    }
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 本地时区偏移（秒）。
fn local_offset(epoch: i64) -> i64 {
    #[cfg(unix)]
    {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = epoch as libc::time_t;
        let ok = unsafe { libc::localtime_r(&t, &mut tm) }.is_null();
        if ok {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
    #[cfg(not(unix))]
    {
        let _ = epoch;
        0
    }
}

/// 今天的本地年月日。
fn today_civil(now: i64) -> (i64, i64, i64) {
    let offset = local_offset(now);
    let local = now + offset;
    let days = local.div_euclid(86400);
    civil_from_days(days)
}

/// 本地某天的 00:00:00 对应的 epoch 秒。
fn start_of_day(y: i64, m: i64, d: i64, now: i64) -> i64 {
    let offset = local_offset(now);
    days_from_civil(y, m, d) * 86400 - offset
}

/// 民用日历 → 天数（Howard Hinnant 的算法）。
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 天数 → 民用日历。
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------- 入口

/// 解析一条查询。
pub fn parse(input: &str) -> Option<Query> {
    let toks = lex(input);
    let mut p = Parser {
        toks,
        pos: 0,
        pending: Pending::default(),
        warnings: Vec::new(),
        only_files: false,
        only_folders: false,
    };
    let root = p.parse_or()?;
    Some(Query {
        root,
        only_files: p.only_files,
        only_folders: p.only_folders,
        warnings: p.warnings,
    })
}

/// 取出命中的区间（相对 name 或 path）。
pub fn highlight_ranges(query: &Query, e: &IndexEntry) -> (Vec<(usize, usize)>, Vec<(usize, usize)>) {
    let mut name_ranges = Vec::new();
    let mut path_ranges = Vec::new();
    collect_ranges(&query.root, e, &mut name_ranges, &mut path_ranges);
    name_ranges.sort_unstable();
    name_ranges.dedup();
    path_ranges.sort_unstable();
    path_ranges.dedup();
    (name_ranges, path_ranges)
}

fn collect_ranges(
    node: &Node,
    e: &IndexEntry,
    name_ranges: &mut Vec<(usize, usize)>,
    path_ranges: &mut Vec<(usize, usize)>,
) {
    match node {
        Node::And(a, b) => {
            collect_ranges(a, e, name_ranges, path_ranges);
            collect_ranges(b, e, name_ranges, path_ranges);
        }
        Node::Or(a, b) => {
            // OR：只保留**真正命中那个分支**的区间
            let mut na = Vec::new();
            let mut pa = Vec::new();
            let mut nb = Vec::new();
            let mut pb = Vec::new();
            collect_ranges(a, e, &mut na, &mut pa);
            collect_ranges(b, e, &mut nb, &mut pb);
            if eval(a, e) {
                name_ranges.extend(na);
                path_ranges.extend(pa);
            } else {
                name_ranges.extend(nb);
                path_ranges.extend(pb);
            }
        }
        Node::Not(_) => {}
        Node::Text(t) => {
            let (text, ranges) = match t.target {
                Target::Name => (e.name.as_str(), t.pattern.ranges(&e.name, t.case_sensitive)),
                Target::Path => (e.path.as_str(), t.pattern.ranges(&e.path, t.case_sensitive)),
            };
            let _ = text;
            match t.target {
                Target::Name => name_ranges.extend(ranges),
                Target::Path => path_ranges.extend(ranges),
            }
        }
        Node::Field(_) | Node::Kind { .. } => {}
    }
}

/// 查询里用到的字段名集合（用于「用了哪些能力」的统计与测试）。
pub fn used_fields(input: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for t in lex(input) {
        if let Tok::Word(w) = t {
            if syntax::FIELDS.contains(&w.to_lowercase().as_str()) {
                out.insert(w.to_lowercase());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, ext: &str, size: i64, mtime: i64, folder: bool) -> IndexEntry {
        IndexEntry {
            name: name.to_string(),
            path: format!("/Users/me/Documents/{name}"),
            dir: "/Users/me/Documents".to_string(),
            extension: ext.to_string(),
            size,
            mtime,
            is_folder: folder,
            depth: 1,
            child_count: 0,
            folded_name: fold(name),
            folded_path: fold(&format!("/Users/me/Documents/{name}")),
        }
    }

    fn matches(query: &str, e: &IndexEntry) -> bool {
        let q = parse(query).expect("应该能解析");
        q.matches(e)
    }

    #[test]
    fn plain_substring_is_case_insensitive() {
        let e = entry("Report.pdf", "pdf", 100, 0, false);
        assert!(matches("report", &e), "小写查询应命中");
        // smart case：查询里有大写 → 区分大小写
        assert!(matches("Report", &e));
        assert!(!matches("REPORT", &e), "smart case 下 REPORT 不该命中 Report.pdf");
        // 显式 nocase 可以覆盖 smart case
        assert!(matches("nocase:REPORT", &e));
    }

    #[test]
    fn ext_field_matches_extension() {
        let pdf = entry("a.pdf", "pdf", 100, 0, false);
        let txt = entry("b.txt", "txt", 100, 0, false);
        assert!(matches("ext:pdf", &pdf));
        assert!(!matches("ext:pdf", &txt));
        assert!(matches("ext:pdf;txt", &txt));
    }

    #[test]
    fn size_field_is_decimal() {
        let big = entry("big.bin", "bin", 20_000_000, 0, false);
        let small = entry("small.bin", "bin", 500, 0, false);
        assert!(matches("size:>10M", &big));
        assert!(!matches("size:>10M", &small));
        assert!(matches("size:<1K", &small));
    }

    #[test]
    fn boolean_operators_work() {
        let e = entry("alpha.txt", "txt", 100, 0, false);
        assert!(matches("alpha AND txt", &e));
        assert!(matches("alpha OR zzz", &e));
        assert!(!matches("NOT alpha", &e));
        assert!(matches("(alpha OR zzz) AND txt", &e));
        assert!(!matches("(alpha OR zzz) AND zzz", &e));
    }

    #[test]
    fn quoted_phrase_is_literal() {
        let e = entry("my file.txt", "txt", 100, 0, false);
        // 引号短语 = 原样的一段（空格不是分隔符）
        assert!(matches("\"my file.txt\"", &e));
        // 引号短语默认仍是【子串】匹配
        assert!(matches("\"my file\"", &e));
        // 要整串相等得显式 exact:
        assert!(matches("exact:\"my file.txt\"", &e));
        assert!(!matches("exact:\"my file\"", &e));
    }

    #[test]
    fn wildcard_matches_whole_name() {
        let e = entry("photo.png", "png", 100, 0, false);
        assert!(matches("*.png", &e));
        assert!(!matches("*.jpg", &e));
        assert!(matches("*", &e));
    }

    #[test]
    fn path_field_searches_the_whole_path() {
        let e = entry("a.txt", "txt", 100, 0, false);
        assert!(matches("path:Documents", &e));
        assert!(!matches("path:Downloads", &e));
    }

    #[test]
    fn unknown_field_produces_a_warning() {
        let q = parse("bogus:x").unwrap();
        assert!(!q.warnings.is_empty());
        assert!(q.warnings[0].contains("bogus"));
    }

    #[test]
    fn file_and_folder_filters() {
        let f = entry("a.txt", "txt", 100, 0, false);
        let d = entry("dir", "", 0, 0, true);
        assert!(matches("file:a", &f));
        assert!(!matches("folder:a", &f));
        assert!(matches("folder:dir", &d));
    }

    #[test]
    fn depth_field() {
        let mut e = entry("a.txt", "txt", 100, 0, false);
        e.depth = 1;
        assert!(matches("depth:1", &e));
        assert!(!matches("depth:2", &e));
    }

    #[test]
    fn highlight_ranges_point_at_the_original_string() {
        let e = entry("Report.pdf", "pdf", 100, 0, false);
        let q = parse("report").unwrap();
        let (name_ranges, _) = highlight_ranges(&q, &e);
        assert_eq!(name_ranges.len(), 1);
        let (s, t) = name_ranges[0];
        assert_eq!(&e.name[s..t], "Report");
    }

    #[test]
    fn date_ranges_parse() {
        let now = now_epoch();
        let (from, to) = parse_date_range("today").unwrap();
        assert!(from <= now && now <= to);
        let (from, to) = parse_date_range("2026-10-01..2026-10-23").unwrap();
        assert!(from < to);
        assert!((to - from) >= 22 * 86400);
        let (from, to) = parse_date_range("2026").unwrap();
        assert!(to - from >= 364 * 86400);
    }

    #[test]
    fn size_parsing_handles_units() {
        assert_eq!(parse_size("500"), Some(500));
        assert_eq!(parse_size("1K"), Some(1000));
        assert_eq!(parse_size("10M"), Some(10_000_000));
        assert_eq!(parse_size("1.5M"), Some(1_500_000));
        assert_eq!(parse_size("2GB"), Some(2_000_000_000));
        assert_eq!(parse_size("abc"), None);
    }

    #[test]
    fn regex_search_is_used_not_full_match() {
        let e = entry("gamma.txt", "txt", 100, 0, false);
        assert!(matches("regex:\"^gam\"", &e));
        assert!(!matches("regex:\"^xyz\"", &e));
    }

    #[test]
    fn civil_day_maths_round_trips() {
        for (y, m, d) in [(1970, 1, 1), (2026, 10, 23), (2000, 2, 29), (1999, 12, 31)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
    }
}
