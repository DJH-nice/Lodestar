fn main() {
    tauri_build::build();

    // 【为什么这里曾经有一段 rpath 设置】
    //   旧版本把上游 C 引擎编成 `build-shim/libfsearch_shim.dylib`，
    //   运行时靠 rpath 找到它。现在引擎是**纯 Rust**（`search-core` crate），
    //   静态链接进二进制 → **不再有外部 dylib 要找**。
    //   → 整段 rpath 逻辑删掉，打包产物也不再塞任何 Frameworks。
    //
    // 【这是「轻量」的一大步】
    //   旧 .app 60M：Frameworks 42M（其中 ICU 34M）+ Resources 14M（GPL 源码包 13M）。
    //   新 .app 不再有那 42M —— 引擎是 Rust 的，Unicode 由纯 Rust 的
    //   `unicode-normalization` + `std::to_lowercase` 承担，不需要 ICU 数据文件。
}
