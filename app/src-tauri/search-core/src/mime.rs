//! MIME 类型 —— `contenttype:` 字段用。
//!
//! 【为什么自己做一张表，而不是问系统】
//!   上游用的是 GIO 的**内容嗅探**（读文件头判断类型）—— 上游自己的文档就写着
//!   「Determining the content type of a file is an expensive operation」。
//!   我们只在**索引期**按扩展名查一次表（O(1)，不在查询热路径上），
//!   查询期只是字符串比较 —— 代价可以忽略。
//!
//! 【表外的扩展名怎么办】
//!   统一当 `application/octet-stream`（二进制）——
//!   宁可「不像是文本」，也不要把它算成 `text/*`
//!   （那会让 `contenttype:text` 乱命中一堆二进制文件）。
//!
//! 【目录的 MIME】
//!   与 GIO 一致：`inode/directory`。

use crate::types::IndexEntry;

/// 目录的 MIME 类型。
pub const DIRECTORY_MIME: &str = "inode/directory";

/// 认不出来的扩展名 → 二进制。
pub const OCTET_STREAM: &str = "application/octet-stream";

/// 这个条目的 MIME 类型。
pub fn mime_of(e: &IndexEntry) -> &'static str {
    if e.is_folder {
        DIRECTORY_MIME
    } else {
        mime_for_extension(&e.extension)
    }
}

/// 扩展名 → MIME 类型（扩展名**不带点**，小写）。
pub fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        // ---- 文本 ----
        "txt" | "text" | "log" | "ini" | "cfg" | "conf" | "list" | "diff" | "patch"
        | "env" | "properties" | "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "m" | "mm"
        | "rs" | "go" | "java" | "kt" | "kts" | "swift" | "scala" | "clj" | "ex" | "exs"
        | "erl" | "hs" | "lua" | "pl" | "pm" | "r" | "rb" | "py" | "pyw" | "php" | "ts"
        | "tsx" | "jsx" | "vue" | "svelte" | "dart" | "zig"
        | "nim" | "jl" | "asm" | "s" => "text/plain",
        "md" | "markdown" | "mdx" => "text/markdown",
        "html" | "htm" | "xhtml" => "text/html",
        "css" => "text/css",
        "csv" | "tsv" => "text/csv",
        "xml" | "xsd" | "xsl" | "plist" => "text/xml",
        "yaml" | "yml" => "text/x-yaml",
        "toml" => "text/x-toml",
        "sh" | "bash" | "zsh" | "fish" | "ksh" => "text/x-shellscript",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "rtf" => "application/rtf",
        "json" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "js" | "mjs" | "cjs" => "text/javascript",

        // ---- 文档 ----
        "pdf" => "application/pdf",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "dotx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroenabled.12",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "pages" => "application/vnd.apple.pages",
        "numbers" => "application/vnd.apple.numbers",
        "key" => "application/vnd.apple.keynote",
        "epub" => "application/epub+zip",
        "mobi" => "application/x-mobipocket-ebook",
        "sqlite" | "sqlite3" | "db" => "application/vnd.sqlite3",

        // ---- 图片 ----
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "icns" => "image/x-icns",
        "psd" => "image/vnd.adobe.photoshop",

        // ---- 音频 ----
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "aif" | "aiff" => "audio/x-aiff",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "mid" | "midi" => "audio/midi",

        // ---- 视频 ----
        "mp4" | "m4v" => "video/mp4",
        "mov" | "qt" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mpg" | "mpeg" => "video/mpeg",
        "wmv" => "video/x-ms-wmv",
        "flv" => "video/x-flv",
        "m2ts" => "video/mp2t",

        // ---- 压缩包 ----
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "zst" => "application/zstd",
        "tgz" => "application/gzip",

        // ---- 安装包 / 可执行 ----
        "dmg" => "application/x-apple-diskimage",
        "pkg" => "application/vnd.apple.installer+xml",
        "exe" => "application/x-msdownload",
        "msi" => "application/x-msdos-program",
        "app" => "application/x-mac-app",
        "deb" => "application/vnd.debian.binary-package",
        "rpm" => "application/x-rpm",

        // ---- 字体 ----
        "ttf" | "otf" | "ttc" => "font/ttf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",

        // ---- 其它 ----
        "iso" => "application/x-iso9660-image",
        "torrent" => "application/x-bittorrent",
        "wasm" => "application/wasm",
        "bin" | "dat" | "img" => OCTET_STREAM,

        _ => OCTET_STREAM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_types_map_to_their_mime() {
        assert_eq!(mime_for_extension("txt"), "text/plain");
        assert_eq!(mime_for_extension("md"), "text/markdown");
        assert_eq!(mime_for_extension("pdf"), "application/pdf");
        assert_eq!(mime_for_extension("png"), "image/png");
        assert_eq!(mime_for_extension("jpg"), "image/jpeg");
        assert_eq!(mime_for_extension("mp4"), "video/mp4");
        assert_eq!(mime_for_extension("mp3"), "audio/mpeg");
        assert_eq!(mime_for_extension("zip"), "application/zip");
    }

    #[test]
    fn unknown_extension_is_binary() {
        assert_eq!(mime_for_extension("zzzz"), OCTET_STREAM);
        assert_eq!(mime_for_extension(""), OCTET_STREAM);
    }

    #[test]
    fn text_family_is_recognizable_by_prefix() {
        // `contenttype:text` 要能命中所有文本类文件
        for ext in ["txt", "md", "html", "css", "csv", "xml", "yaml", "sh", "rs"] {
            assert!(
                mime_for_extension(ext).starts_with("text/"),
                "{ext} 不该是 text/*（实际 {}）",
                mime_for_extension(ext)
            );
        }
        // 二进制类不该被 text 命中
        for ext in ["png", "mp4", "zip", "pdf"] {
            assert!(
                !mime_for_extension(ext).starts_with("text/"),
                "{ext} 不该是 text/*"
            );
        }
    }
}
