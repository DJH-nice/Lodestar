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
use crate::mime;
use crate::syntax::{self, ModifierKind};
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
    Size { filter: NumFilter },
    /// `dm:today` / `dm:2026-10-01..2026-10-23`
    Mtime { from: i64, to: i64 },
    /// `depth:1`（`parents:1` 是同一个东西）
    Depth { filter: NumFilter },
    /// `empty:` —— 空文件或空目录
    Empty,
    /// `childcount:1` —— 子项总数（文件 + 目录）
    ChildCount { filter: NumFilter },
    /// `childfilecount:1` —— 子文件数
    ChildFileCount { filter: NumFilter },
    /// `childfoldercount:1` —— 子目录数
    ChildFolderCount { filter: NumFilter },
    /// `parent:/Users/me/Downloads` —— 父目录路径相等
    Parent { needle: String },
    /// `contenttype:text` —— 按 MIME 类型
    ContentType { needle: String },
    /// `pic:` / `audio:` / `video:` / `doc:` / `archive:` / `app:`
    Group { name: String },
}

/// 数值条件 —— 单值比较或区间。
///
/// 【为什么要有区间】上游的文档里就写着 `childfoldercount:10..20`、
///   `childfilecount:<=4` —— 用户照抄就不该搜不到。
#[derive(Debug, Clone, Copy)]
pub enum NumFilter {
    /// `=N` `>N` `>=N` `<N` `<=N`
    Cmp(CmpOp, i64),
    /// `A..B` —— 闭区间（含两端）
    Range(i64, i64),
}

impl NumFilter {
    fn matches(&self, v: u32) -> bool {
        self.matches_i64(v as i64)
    }

    fn matches_i64(&self, v: i64) -> bool {
        match self {
            NumFilter::Cmp(op, b) => cmp_i64(v, *op, *b),
            NumFilter::Range(lo, hi) => v >= *lo && v <= *hi,
        }
    }
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
    /// **什么都不匹配**（未知字段）—— 与上游一致。
    ///
    /// 【为什么不是「匹配一切」】
    ///   未知字段时若匹配一切，用户会看到满屏结果 + 一行警告 →
    ///   很容易误以为「搜索正常，就是文件多」。
    ///   0 条 + 明确的警告，才是「你写错了」的正确反馈。
    Nothing,
    /// **匹配一切**（查询还没写完时的兜底）。
    ///
    /// 【为什么不用「空文本节点」】空文本节点在**高亮**时会命中每个位置
    ///   （`match_indices("")`）→ 整个文件名被标黄，看着像「全文命中」。
    ///   这个节点不产生任何高亮区间。
    Everything,
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
        Node::Nothing => false,
        Node::Everything => true,
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
        FieldTerm::Size { filter } => filter.matches_i64(e.size),
        FieldTerm::Mtime { from, to } => e.mtime >= *from && e.mtime <= *to,
        FieldTerm::Depth { filter } => filter.matches(e.depth),
        FieldTerm::Empty => {
            if e.is_folder {
                e.child_count == 0
            } else {
                e.size == 0
            }
        }
        FieldTerm::ChildCount { filter } => filter.matches(e.child_count),
        FieldTerm::ChildFileCount { filter } => filter.matches(e.child_file_count),
        FieldTerm::ChildFolderCount { filter } => filter.matches(e.child_folder_count),
        FieldTerm::Parent { needle } => fold(&e.dir) == *needle,
        FieldTerm::ContentType { needle } => {
            // 按 MIME 类型匹配（上游也是拿 content type 串做「包含」匹配）
            //
            // 【为什么还认内置的类型组名】`contenttype:pic` 这种「混用两套写法」
            //   如果只按 MIME 匹配 → 一个都搜不到、而且一声不吭。
            //   → 顺便也认 `pic` / `doc` / `archive` 这些组名。
            mime::mime_of(e).contains(needle.as_str()) || group_matches(needle, e)
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

/// 扩展名分组（我们自己定的表 —— 不是上游的）。
///
/// 【为什么目录也要参与】macOS 上 `.app` 是**目录** ——
///   扫描器给目录的扩展名是空串，不这样的话 `app:` 一个应用包都匹配不到
///   （而 .app 恰恰是 macOS 上最主要的「应用」形态）。
fn group_matches(name: &str, e: &IndexEntry) -> bool {
    let ext = effective_extension(e);
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
    // 大小写不敏感（目录的扩展名是从原名切出来的，没做小写化）
    table.iter().any(|t| t.eq_ignore_ascii_case(ext))
}

/// 条目参与分组匹配用的扩展名。
///
/// 【规则】文件 → 索引期算好的扩展名；目录 → 名字里最后一个点之后的部分
///   （`"Foo.app"` → `"app"`；没有点、或点在开头 → 空串）。
fn effective_extension(e: &IndexEntry) -> &str {
    if !e.is_folder {
        return e.extension.as_str();
    }
    match e.name.rfind('.') {
        Some(i) if i > 0 && i + 1 < e.name.len() => &e.name[i + 1..],
        _ => "",
    }
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
        //
        // 【为什么必须成对识别】运算符表里写着 `<=` / `>=` ——
        //   只认单个字符的话，`size:<=1K` 会被切成 `<` `=` `1K` 三个 token，
        //   值直接读不出来 → 用户照抄运算符表里的写法却搜不到（表≠实现）。
        if matches!(c, '<' | '>' | '=' | '!' | '&' | '|') {
            let mut op = String::new();
            op.push(c);
            i += 1;
            if i < chars.len() {
                let next = chars[i];
                let is_pair = matches!(
                    (c, next),
                    ('<', '=')
                        | ('>', '=')
                        | ('!', '=')
                        | ('<', '<')
                        | ('>', '>')
                        | ('=', '=')
                        | ('&', '&')
                        | ('|', '|')
                );
                if is_pair {
                    op.push(next);
                    i += 1;
                }
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
    /// 当前开着几层括号。
    ///
    /// 【为什么需要它】`parse_and` 看到右括号时，分不清它是
    ///   「闭合本组的那个」还是「多出来的那一个」——
    ///   前者完全正常（`(a OR b) AND c`），后者是笔误。
    ///   → 靠深度区分：**在括号里面**（depth > 0）的右括号是本组的闭合符，
    ///   不该报警告；只有**顶层**落单的右括号才是笔误。
    group_depth: u32,
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
                    match self.parse_and() {
                        Some(right) => left = Node::Or(Box::new(left), Box::new(right)),
                        None => {
                            // OR 后面还没写条件 —— 用户还在打字
                            self.warnings
                                .push("OR 后面还没有条件（还没写完）".to_string());
                        }
                    }
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
                    match self.parse_not() {
                        Some(right) => left = Node::And(Box::new(left), Box::new(right)),
                        None => {
                            // AND 后面还没写条件 —— 用户还在打字。
                            // 【为什么不能整条失败】已经写好的 left 是有效的
                            //   （"alpha AND" 应该看到 alpha 的结果），
                            //   丢掉它 = 用户以为搜不到东西。
                            self.warnings
                                .push("AND 后面还没有条件（还没写完）".to_string());
                        }
                    }
                }
                Some(Tok::Word(_)) | Some(Tok::Quoted(_)) | Some(Tok::LParen) => {
                    let right = self.parse_not()?;
                    left = Node::And(Box::new(left), Box::new(right));
                }
                Some(Tok::Op(op)) => {
                    // 落单的运算符（比如 `size!=1M` 里的 `!=`）——
                    // 【为什么不能悄悄丢掉】丢掉 = 这一段条件**完全失效**
                    //   而界面一声不吭 → 用户会以为「搜过了，没有就是没有」。
                    //   → 明说看不懂。
                    //
                    // 【注意】AND / OR 是上一层（parse_and / parse_or）消费的，
                    //   到这里说明是**组合**写法 —— 不是错误，别报警告。
                    if !matches!(op.as_str(), "AND" | "&&" | "OR" | "||") {
                        self.warnings.push(format!("看不懂的运算符：{op}"));
                    }
                    break;
                }
                Some(Tok::RParen) => {
                    // 多出来的右括号（`alpha) beta`）—— 后面那段会被丢掉，
                    // 不说的话用户会以为「alpha 而且 beta」都生效了。
                    //
                    // 【注意】**在括号里面**看到的右括号是本组的闭合符
                    //   （`(a OR b) AND c`）—— 那是正常的，不能报警告。
                    if self.group_depth == 0 {
                        self.warnings
                            .push("括号没配对（多了一个右括号）".to_string());
                    }
                    break;
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
                match self.parse_not() {
                    Some(inner) => Some(Node::Not(Box::new(inner))),
                    None => {
                        // NOT 后面还没写东西 —— 用户还在打字
                        self.warnings
                            .push("NOT 后面还没有条件（还没写完）".to_string());
                        Some(Node::Everything)
                    }
                }
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Option<Node> {
        match self.next() {
            Some(Tok::LParen) => {
                // 注意：左括号已经被上面的 `self.next()` 消费掉了 ——
                //   这里**不能再 next() 一次**，否则会把组里的第一个词吃掉。
                self.group_depth += 1;
                let inner = self.parse_or();
                // 吃掉右括号（没有也不报错 —— 用户打到一半是很常见的）
                if matches!(self.peek(), Some(Tok::RParen)) {
                    self.next();
                    self.group_depth -= 1;
                } else {
                    // 括号没闭合 —— 提示一句，但**里面已经解析出来的部分照常用**
                    self.warnings
                        .push("括号没闭合（还没写完）".to_string());
                }
                match inner {
                    Some(node) => Some(node),
                    None => {
                        // 括号里还什么都没有 —— 用户还在打字
                        self.warnings
                            .push("括号里还没有内容（还没写完）".to_string());
                        Some(Node::Everything)
                    }
                }
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

            // 修饰符 + **字段**（`file:size:>10M`）——
            //   修饰符作用在这个字段上，不能把字段名当成「被修饰的词」吃掉
            //   （上游的写法，也是帮助面板里的例子）。
            if let Some(kind) = syntax::modifier_kind(&lower) {
                if self.value_is_field() {
                    self.apply_query_switch(kind);
                    return self.parse_not();
                }
            }

            // 值可能是运算符 + 词，或直接是词/引号
            let Some((op, value, quoted)) = self.read_value() else {
                // 写法不支持（目前只有 `!=`）→ 什么都不匹配 + 已经有警告
                return Some(Node::Nothing);
            };
            return Some(self.make_field_or_modifier(lower, op, value, quoted));
        }

        // 裸修饰符
        if let Some(kind) = syntax::modifier_kind(&lower) {
            match kind {
                ModifierKind::FilesOnly => {
                    self.apply_query_switch(kind);
                    return Some(Node::Kind { folders: false });
                }
                ModifierKind::FoldersOnly => {
                    self.apply_query_switch(kind);
                    return Some(Node::Kind { folders: true });
                }
                ModifierKind::NotFilesOnly | ModifierKind::NotFoldersOnly => {
                    // 取消「只看文件 / 只看目录」—— 上游是 REMOVE_FLAG，
                    // 我们对应的是「把查询级的开关关掉」。
                    self.apply_query_switch(kind);
                }
                ModifierKind::Case => self.pending.case_sensitive = Some(true),
                ModifierKind::NoCase => self.pending.case_sensitive = Some(false),
                ModifierKind::Exact => self.pending.exact = true,
                ModifierKind::Regex => self.pending.regex = true,
                ModifierKind::Path => self.pending.target = Some(Target::Path),
                ModifierKind::NoPath => self.pending.target = Some(Target::Name),
                ModifierKind::NoRegex => {}
            }
            // 裸修饰符本身不产生节点 —— 它作用于下一个词。
            // 后面没有词时（查询就写了「case」）也不能让整个查询解析失败 →
            // 兜一个「匹配一切」的节点。
            return self.parse_not().or(Some(Node::Everything));
        }

        Some(self.make_text(word, false))
    }

    /// 下一个词是不是「字段名 + 冒号」（`file:size:>10M` 里的 `size:`）。
    fn value_is_field(&self) -> bool {
        match (self.toks.get(self.pos), self.toks.get(self.pos + 1)) {
            (Some(Tok::Word(w)), Some(Tok::Colon)) => {
                syntax::field_kind(&w.to_lowercase()).is_some()
            }
            _ => false,
        }
    }

    /// 修饰符里「查询级开关」的那部分（只看文件 / 只看目录）。
    ///
    /// 【为什么是查询级】`file:` 在我们这儿是**整个查询**的开关
    ///   （上游是逐节点的标志）—— 速查卡与手册都按这个语义写的。
    fn apply_query_switch(&mut self, kind: ModifierKind) {
        match kind {
            ModifierKind::FilesOnly => self.only_files = true,
            ModifierKind::NotFilesOnly => self.only_files = false,
            ModifierKind::FoldersOnly => self.only_folders = true,
            ModifierKind::NotFoldersOnly => self.only_folders = false,
            _ => {}
        }
    }

    /// 读 `字段:` 后面的值。
    ///
    /// 【返回值】`None` = 这个写法引擎不支持（调用方给警告 + 什么都不匹配）。
    ///   目前只有 `!=`（我们没实现「不等于」；用 `NOT` 代替）。
    fn read_value(&mut self) -> Option<(CmpOp, String, bool)> {
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
                    // 【为什么不实现】「不等于」对每个字段的语义都不一样
                    //   （ext 是「不在列表里」，size 是「不等于」，区间怎么排除？）。
                    //   而运算符表里没有它 → 不宣传就不会有人照抄。
                    self.next();
                    self.warnings
                        .push("暂不支持 != 运算符（请用 NOT 代替）".to_string());
                    return None;
                }
                _ => {}
            }
        }
        match self.next() {
            Some(Tok::Word(w)) => Some((op, w, false)),
            Some(Tok::Quoted(q)) => Some((op, q, true)),
            _ => Some((op, String::new(), false)),
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
        if let Some(kind) = syntax::modifier_kind(&name) {
            // 「只看文件 / 只看目录」是**查询级开关**（上游也是这个模型）。
            // `nofileonly:` 的作用就是把这个开关**关掉**（上游是 REMOVE_FLAG）。
            self.apply_query_switch(kind);
            let mut term = self.make_text(value.clone(), quoted);
            if let Node::Text(t) = &mut term {
                match kind {
                    ModifierKind::Exact => t.exact = true,
                    ModifierKind::Case => t.case_sensitive = true,
                    ModifierKind::NoCase => t.case_sensitive = false,
                    ModifierKind::Path => t.target = Target::Path,
                    ModifierKind::NoPath => t.target = Target::Name,
                    ModifierKind::Regex => {
                        if let Pattern::Plain { .. } = &t.pattern {
                            t.pattern = build_regex(&value, false, true);
                        }
                    }
                    _ => {}
                }
            }
            return term;
        }

        // 字段 —— 表里没有 = 引擎不认（**不会静默当普通词**）
        let kind = match syntax::field_kind(&name) {
            Some(k) => k,
            None => {
                self.warnings.push(format!("未知字段：{name}"));
                return Node::Nothing;
            }
        };

        let folded_value = fold(&value);
        let term = match kind {
            syntax::FieldKind::Ext => {
                let values: Vec<String> = value
                    .split(';')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect();
                FieldTerm::Ext { values }
            }
            syntax::FieldKind::Size => match parse_size_filter(op, &value) {
                Some(filter) => FieldTerm::Size { filter },
                None => {
                    self.warnings.push(format!("看不懂的大小：{value}"));
                    FieldTerm::Size {
                        filter: NumFilter::Cmp(CmpOp::Lt, 0),
                    }
                }
            },
            syntax::FieldKind::Mtime => match parse_date_range(&value) {
                Some((from, to)) => FieldTerm::Mtime { from, to },
                None => {
                    self.warnings.push(format!("看不懂的日期：{value}"));
                    FieldTerm::Mtime { from: 0, to: 0 }
                }
            },
            syntax::FieldKind::Depth => match parse_num_filter(op, &value) {
                Some(filter) => FieldTerm::Depth { filter },
                None => {
                    self.warnings.push(format!("看不懂的深度：{value}"));
                    FieldTerm::Depth {
                        filter: NumFilter::Cmp(CmpOp::Lt, 0),
                    }
                }
            },
            syntax::FieldKind::Empty => FieldTerm::Empty,
            syntax::FieldKind::ChildCount => match parse_num_filter(op, &value) {
                Some(filter) => FieldTerm::ChildCount { filter },
                None => {
                    self.warnings.push(format!("看不懂的子项数：{value}"));
                    FieldTerm::ChildCount {
                        filter: NumFilter::Cmp(CmpOp::Lt, 0),
                    }
                }
            },
            syntax::FieldKind::ChildFileCount => match parse_num_filter(op, &value) {
                Some(filter) => FieldTerm::ChildFileCount { filter },
                None => {
                    self.warnings.push(format!("看不懂的子文件数：{value}"));
                    FieldTerm::ChildFileCount {
                        filter: NumFilter::Cmp(CmpOp::Lt, 0),
                    }
                }
            },
            syntax::FieldKind::ChildFolderCount => match parse_num_filter(op, &value) {
                Some(filter) => FieldTerm::ChildFolderCount { filter },
                None => {
                    self.warnings.push(format!("看不懂的子目录数：{value}"));
                    FieldTerm::ChildFolderCount {
                        filter: NumFilter::Cmp(CmpOp::Lt, 0),
                    }
                }
            },
            syntax::FieldKind::Parent => FieldTerm::Parent { needle: folded_value },
            syntax::FieldKind::ContentType => {
                if value.trim().is_empty() {
                    // 上游的空 contenttype 是「匹配一切」，我们也这样，
                    // 但**给个提示** —— 用户多半是忘了写类型名。
                    self.warnings
                        .push("contenttype: 需要一个类型名（比如 contenttype:text）".to_string());
                }
                FieldTerm::ContentType { needle: folded_value }
            }
            syntax::FieldKind::Group => {
                if !value.trim().is_empty() {
                    // 【为什么要警告】冒号后面的值会被**忽略** ——
                    //   不说的话，用户会以为 `pic:png` 是「png 这类图片」，
                    //   实际上它等于 `pic:`（所有图片）。
                    self.warnings.push(format!(
                        "{name}: 后面不要写值（要按扩展名筛请用 ext:）"
                    ));
                }
                FieldTerm::Group { name: name.clone() }
            }
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

/// 解析数值条件：`1` `>10` `<=4` `10..20`。
///
/// 【为什么支持区间】上游文档里就写着 `childfoldercount:10..20`、
///   `childfilecount:<=4` —— 用户照抄就不该搜不到。
fn parse_num_filter(op: CmpOp, value: &str) -> Option<NumFilter> {
    let t = value.trim();
    if t.is_empty() {
        return None;
    }
    if let Some((a, b)) = t.split_once("..") {
        let lo = a.trim().parse::<i64>().ok()?;
        let hi = b.trim().parse::<i64>().ok()?;
        if lo > hi {
            return None;
        }
        return Some(NumFilter::Range(lo, hi));
    }
    Some(NumFilter::Cmp(op, t.parse::<i64>().ok()?))
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
        // 【为什么允许一边是空的】`dm:2024..`（2024 年之后）、`dm:..2024`（2024 年之前）
        //   是手册与速查卡里都写着的写法 —— 空的一边就是「到头」。
        let from = match a.trim() {
            "" => i64::MIN,
            _ => parse_date_point(a.trim(), false)?,
        };
        let to = match b.trim() {
            "" => i64::MAX,
            _ => parse_date_point(b.trim(), true)?,
        };
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

    // 精确到分钟 / 秒：`2026-10-22 12:00`（值里有空格，所以要加引号）
    //
    // 【为什么要支持】手册与速查卡都写着「要精确到分钟得加引号：
    //   dm:"2026-10-22 12:00"」—— 不支持的话就是**文档骗人**。
    if let Some((date_part, time_part)) = t.split_once(char::is_whitespace) {
        let date_part = date_part.trim();
        let time_part = time_part.trim();
        if !date_part.is_empty() && !time_part.is_empty() {
            if let Some((offset, precise)) = parse_time_of_day(time_part) {
                let day_start = parse_date_point(date_part, false)?;
                // 「到这一分钟/秒的末尾」：只有写的是 HH:MM 才补满 59 秒
                let tail = if end_of_unit && !precise { 59 } else { 0 };
                return Some(day_start + offset + tail);
            }
        }
    }

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

/// 解析大小条件：`10M` `>10M` `<=1M` `1M..10M`。
///
/// 【为什么区间也要支持】其它数值字段（深度、子项数）都支持区间，
///   大小却不支持 → 用户写 `size:1M..10M` 会得到一句警告，很费解。
fn parse_size_filter(op: CmpOp, value: &str) -> Option<NumFilter> {
    let t = value.trim();
    if t.is_empty() {
        return None;
    }
    if let Some((a, b)) = t.split_once("..") {
        let lo = parse_size(a)?;
        let hi = parse_size(b)?;
        if lo > hi {
            return None;
        }
        return Some(NumFilter::Range(lo, hi));
    }
    Some(NumFilter::Cmp(op, parse_size(t)?))
}

/// 解析「一天里的时刻」→ `(当天 0 点起的秒数, 是否精确到秒)`。
///
/// `12:00` → (43200, false) · `12:00:30` → (43230, true)
fn parse_time_of_day(s: &str) -> Option<(i64, bool)> {
    let t = s.trim();
    let mut parts = t.split(':');
    let h: i64 = parts.next()?.trim().parse().ok()?;
    let m: i64 = parts.next()?.trim().parse().ok()?;
    let sec_part = parts.next();
    if parts.next().is_some() {
        return None; // 多于三段 → 不认
    }
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    match sec_part {
        None => Some((h * 3600 + m * 60, false)),
        Some(sec_s) => {
            let sec: i64 = sec_s.trim().parse().ok()?;
            if !(0..60).contains(&sec) {
                return None;
            }
            Some((h * 3600 + m * 60 + sec, true))
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
///
/// 【为什么永不失败】
///   用户**打字过程中**的输入天然是不完整的：`alpha AND`、单独一个 `(`、
///   单独的 `NOT`…… 以前这些会让整条查询解析失败 → 引擎返回
///   `Err(SearchFailed)` → 界面上只剩一句英文 "search failed"，
///   用户既不知道发生了什么，也不知道该怎么改。
///   → 现在：**缺条件就结束当前分支 + 给一句中文提示**。
///     解析器永远不会失败，引擎也就不会因为语法问题报错。
pub fn parse(input: &str) -> Query {
    let toks = lex(input);
    let mut p = Parser {
        toks,
        pos: 0,
        pending: Pending::default(),
        warnings: Vec::new(),
        only_files: false,
        only_folders: false,
        group_depth: 0,
    };
    let root = match p.parse_or() {
        Some(r) => r,
        None => {
            // 一个 term 都解析不出来（只有一个符号 / 空括号之类）
            if !input.trim().is_empty() {
                p.warnings
                    .push("查询还没写完（或写法引擎不认）".to_string());
            }
            Node::Everything
        }
    };
    Query {
        root,
        only_files: p.only_files,
        only_folders: p.only_folders,
        warnings: p.warnings,
    }
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
        Node::Field(_) | Node::Kind { .. } | Node::Nothing | Node::Everything => {}
    }
}

/// 查询里用到的字段名集合（用于「用了哪些能力」的统计与测试）。
pub fn used_fields(input: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for t in lex(input) {
        if let Tok::Word(w) = t {
            let lower = w.to_lowercase();
            if syntax::field_kind(&lower).is_some() {
                out.insert(lower);
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
            child_file_count: 0,
            child_folder_count: 0,
            folded_name: fold(name),
            folded_path: fold(&format!("/Users/me/Documents/{name}")),
        }
    }

    /// 一个目录条目（带子项计数）。
    fn folder_entry(name: &str, files: u32, folders: u32) -> IndexEntry {
        let mut e = entry(name, "", 0, 0, true);
        e.child_file_count = files;
        e.child_folder_count = folders;
        e.child_count = files + folders;
        e
    }

    fn matches(query: &str, e: &IndexEntry) -> bool {
        let q = parse(query);
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
    fn size_supports_two_character_operators_and_ranges() {
        // 【回归】`<=` / `>=` 写在运算符表里（前端也照着显示），
        //   但词法器以前只把「相同字符」成对识别 → `size:<=5M` 读不出值。
        let mid = entry("mid.bin", "bin", 5_000_000, 0, false);
        assert!(matches("size:<=5M", &mid));
        assert!(!matches("size:<5M", &mid));
        assert!(matches("size:>=5M", &mid));
        // 区间（跟深度、子项数保持一致）
        assert!(matches("size:1M..10M", &mid));
        assert!(!matches("size:1M..10M", &entry("big.bin", "bin", 20_000_000, 0, false)));
        assert!(!matches("size:1M..10M", &entry("small.bin", "bin", 500, 0, false)));
    }

    #[test]
    fn not_equal_is_rejected_with_a_warning() {
        // `!=` 不在运算符表里 → 不宣传；写出来了要给明确警告，不能静默。
        let e = entry("a.txt", "txt", 100, 0, false);
        let q = parse("size!=1M");
        assert!(
            !q.warnings.is_empty(),
            "!= 要有明确警告（否则这一段条件会悄悄失效）"
        );
        assert!(!q.matches(&e), "不支持的写法不该匹配任何东西");
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
        let q = parse("bogus:x");
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
    fn a_modifier_can_prefix_a_field() {
        // 【回归】上游的写法（帮助面板的例子里就有）：`file:size:>20gb`
        //   修饰符作用在**字段**上，不能把字段名当成「被修饰的词」吃掉。
        let big = entry("big.bin", "bin", 20_000_000, 0, false);
        let dir = entry("dir", "", 0, 0, true);
        let q = parse("file:size:>10M");
        assert!(q.warnings.is_empty(), "不该有警告：{:?}", q.warnings);
        assert!(q.only_files, "file: 要打开「只看文件」");
        assert!(q.matches(&big));
        assert!(!q.matches(&dir), "目录不该命中（只看文件）");
    }

    #[test]
    fn a_modifier_prefixing_a_field_does_not_eat_the_field_name() {
        // 修饰符后面跟的词**不是**字段时，它仍然是「被修饰的词」
        let e = entry("ext.txt", "txt", 100, 0, false);
        assert!(matches("exact:ext.txt", &e));
        // 修饰符 + 字段 → 字段照常生效
        let e2 = entry("a.pdf", "pdf", 100, 0, false);
        assert!(matches("file:ext:pdf", &e2));
        assert!(!matches("file:ext:pdf", &entry("a.txt", "txt", 100, 0, false)));
    }

    #[test]
    fn depth_field() {
        let mut e = entry("a.txt", "txt", 100, 0, false);
        e.depth = 1;
        assert!(matches("depth:1", &e));
        assert!(!matches("depth:2", &e));
    }

    #[test]
    fn parents_is_an_alias_of_depth() {
        // 【为什么对齐上游】上游的 `parents` 也解析成 depth ——
        //   帮助面板那一行的说明写的就是「目录深度」。
        //   我们以前把 `parents:` 做成「祖先路径含这一段」，
        //   跟面板的说明对不上 → 现在统一成深度。
        let mut e = entry("a.txt", "txt", 100, 0, false);
        e.depth = 2;
        assert!(matches("parents:2", &e));
        assert!(matches("depth:2", &e));
        assert!(!matches("parents:3", &e));
        // 不是数字 → 跟 depth 一样给警告
        let q = parse("parents:Documents");
        assert!(!q.warnings.is_empty(), "parents: 传非数字要有警告");
    }

    #[test]
    fn child_counts_are_split_by_kind() {
        let d = folder_entry("dir", 2, 1);
        // 总数
        assert!(matches("childcount:3", &d));
        assert!(!matches("childcount:2", &d));
        // 子文件数
        assert!(matches("childfilecount:2", &d));
        assert!(!matches("childfilecount:1", &d));
        // 子目录数
        assert!(matches("childfoldercount:1", &d));
        assert!(!matches("childfoldercount:2", &d));
    }

    #[test]
    fn numeric_fields_accept_ranges_and_comparisons() {
        let d = folder_entry("dir", 2, 1);
        // 区间（上游文档里的写法：childfoldercount:10..20）
        assert!(matches("childfilecount:1..3", &d));
        assert!(!matches("childfilecount:3..5", &d));
        // 比较
        assert!(matches("childfilecount:<=2", &d));
        assert!(!matches("childfilecount:<2", &d));
        assert!(matches("childfilecount:>=2", &d));
        // 区间两端都含
        assert!(matches("childfilecount:2..2", &d));
    }

    #[test]
    fn contenttype_matches_the_mime_type() {
        let txt = entry("a.txt", "txt", 100, 0, false);
        let png = entry("b.png", "png", 100, 0, false);
        let pdf = entry("c.pdf", "pdf", 100, 0, false);
        // 按 MIME 家族
        assert!(matches("contenttype:text", &txt));
        assert!(!matches("contenttype:text", &png));
        assert!(matches("contenttype:image", &png));
        // 按具体类型
        assert!(matches("contenttype:pdf", &pdf));
        assert!(matches("contenttype:application/pdf", &pdf));
        assert!(!matches("contenttype:pdf", &txt));
    }

    #[test]
    fn contenttype_without_a_value_warns() {
        let q = parse("contenttype:");
        assert!(
            !q.warnings.is_empty(),
            "空的 contenttype: 要给提示（多半是忘了写类型名）"
        );
    }

    #[test]
    fn contenttype_also_accepts_the_builtin_group_names() {
        // `contenttype:pic` 是「混用两套写法」——
        //   只按 MIME 匹配的话一个都搜不到、而且一声不吭。
        let png = entry("a.png", "png", 100, 0, false);
        let txt = entry("b.txt", "txt", 100, 0, false);
        assert!(matches("contenttype:pic", &png));
        assert!(!matches("contenttype:pic", &txt));
    }

    #[test]
    fn group_fields_ignore_the_value_but_warn_about_it() {
        let png = entry("a.png", "png", 100, 0, false);
        assert!(matches("pic:", &png));
        // 带值 → 值被忽略，但**必须给警告**（否则用户以为在筛 png）
        let q = parse("pic:png");
        assert!(!q.warnings.is_empty(), "pic:png 必须警告");
        assert!(q.matches(&png), "pic: 本身仍然生效");
    }

    #[test]
    fn app_group_matches_app_bundles_which_are_folders() {
        // macOS 上 .app 是**目录** —— 扫描器给目录的扩展名是空串，
        // 不特殊处理的话 `app:` 一个应用包都搜不到。
        let bundle = entry("Safari.app", "", 0, 0, true);
        assert!(matches("app:", &bundle));
        let exe = entry("setup.exe", "exe", 100, 0, false);
        assert!(matches("app:", &exe));
        let txt = entry("a.txt", "txt", 100, 0, false);
        assert!(!matches("app:", &txt));
    }

    #[test]
    fn nofileonly_cancels_the_files_only_switch() {
        // 名字里都带 "a"（查询词就是 "a"）—— 文件与目录都该命中
        let f = entry("a.txt", "txt", 100, 0, false);
        let d = entry("a-dir", "", 0, 0, true);
        // file: 只看文件
        let q = parse("file:a");
        assert!(q.only_files);
        assert!(q.matches(&f) && !q.matches(&d));
        // nofileonly: 取消这个限制
        let q = parse("nofileonly:a");
        assert!(!q.only_files, "nofileonly: 要把「只看文件」关掉");
        assert!(q.matches(&f) && q.matches(&d));
    }

    #[test]
    fn nofolderonly_cancels_the_folders_only_switch() {
        // 名字里都带 "dir"（查询词就是 "dir"）
        let f = entry("dir.txt", "txt", 100, 0, false);
        let d = entry("dir", "", 0, 0, true);
        let q = parse("nofolderonly:dir");
        assert!(!q.only_folders);
        assert!(q.matches(&f) && q.matches(&d));
    }

    #[test]
    fn unknown_field_matches_nothing_but_warns() {
        // 【为什么是「什么都不匹配」而不是「匹配一切」】
        //   未知字段时给一堆结果 + 一行警告，用户很容易以为「搜索正常」。
        //   0 条 + 警告 = 明确告诉用户「你写错了」。
        let e = entry("a.txt", "txt", 100, 0, false);
        let q = parse("bogus:x");
        assert!(!q.warnings.is_empty());
        assert!(!q.matches(&e), "未知字段不该匹配任何东西");
    }

    #[test]
    fn a_trailing_bare_modifier_does_not_break_the_query() {
        // 查询只有一个裸修饰符 → 不能让整个查询解析失败
        let e = entry("a.txt", "txt", 100, 0, false);
        let q = parse("case");
        assert!(q.warnings.is_empty());
        assert!(q.matches(&e));
    }

    #[test]
    fn highlight_ranges_point_at_the_original_string() {
        let e = entry("Report.pdf", "pdf", 100, 0, false);
        let q = parse("report");
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
    fn open_ended_date_ranges_work() {
        // `dm:2024..`（2024 年之后）/ `dm:..2024`（2024 年之前）
        let (from, to) = parse_date_range("2024..").unwrap();
        assert!(from < now_epoch() && to == i64::MAX);
        let (from, to) = parse_date_range("..2024").unwrap();
        assert!(from == i64::MIN && to < now_epoch());
    }

    #[test]
    fn dates_can_be_precise_to_the_minute() {
        // `dm:"2026-10-22 12:00"` —— 手册与速查卡都写着这个写法
        let (from, to) = parse_date_range("2026-10-22 12:00").unwrap();
        // 单给一个时刻 → 覆盖那一整分钟
        assert_eq!(to - from, 59, "到那一分钟的末尾");
        let (from2, to2) = parse_date_range("2026-10-22 12:00..2026-10-22 13:00").unwrap();
        assert_eq!(from2, from, "区间起点就是那个时刻");
        assert!(to2 > to, "区间右端是 13:00 那分钟的末尾");
        assert!((to2 - from2) >= 3600);
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
