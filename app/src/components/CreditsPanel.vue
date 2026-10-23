<script setup lang="ts">
import { onMounted, ref } from "vue";
import { getVersion } from "@tauri-apps/api/app";

/**
 * **致谢与许可** —— 本软件的法务信息。
 *
 *  【为什么要有这个面板】软件装进 `/Applications` 之后，
 *   绝大多数用户不会再去翻仓库 → 许可与出处**必须能在应用里看见**。
 *
 *  【为什么不叫「关于」】macOS 的应用菜单里已经有系统的「关于 Sextant」
 *   （显示版本号与版权）。再放一个同名项会让人以为是同一个东西。
 *   → 这里叫「致谢与许可」，指向的是**法务信息**（许可 + 第三方组件）。
 *
 *  【引擎是自研的】2026-10-23 起，本软件的搜索引擎内核完全自研
 *   （`app/src-tauri/search-core/`，纯 Rust）——
 *   早期版本用的是上游 FSearch 的 C 引擎（GPL），现已整体替换。
 *   → 下面列的第三方组件都是**构建期依赖**，没有一个进到引擎里。
 *
 *  【内容为什么写死在这里】这些信息**不随运行状态变化** ——
 *   与其每次去查 Tauri 的版本号、构建元数据，不如写成静态事实。
 *   只有版本号是运行时取的（见下方 version）。
 */
const emit = defineEmits<{ (e: "close"): void }>();

/** 版本号 —— 从 Tauri 的 bundle 元数据里取。
 *
 *  【为什么要 fallback】不在 Tauri 环境里（单元测试、浏览器里直接开 vite dev）
 *   时 `getVersion()` 会抛 → 用一个占位串，**不让面板开不起来**。 */
const version = ref("—");
onMounted(async () => {
  try {
    version.value = await getVersion();
  } catch {
    /* 不在 Tauri 环境里 —— 保持占位符。 */
  }
});
</script>

<template>
  <div class="panel-drawer-backdrop" @click.self="emit('close')">
    <section class="panel-drawer" @click.stop>
      <header class="panel-head">
        <h2>致谢与许可</h2>
        <button class="panel-close" @click="emit('close')">×</button>
      </header>

      <div class="panel-body credits-body">
        <h3>Sextant</h3>
        <p class="credits-sub">
          版本 {{ version }} · macOS 上的文件搜索器
        </p>

        <h3>搜索引擎</h3>
        <p>
          本软件的搜索引擎内核是<strong>完全自研</strong>的
          （<code>app/src-tauri/search-core/</code>，纯 Rust）——
          并行扫描、索引存储、查询语法、排序分页全部是本项目自己的实现，
          <strong>不含任何第三方搜索引擎的代码</strong>。
        </p>
        <p class="credits-note">
          界面同样是自研的：Tauri v2（Rust）+ Vue 3 + TypeScript。
        </p>

        <h3>许可证</h3>
        <p>
          本软件整体按
          <strong>MIT 许可</strong>发布
          （Copyright © 2026 Sextant contributors）。
        </p>
        <p class="credits-note">
          许可证全文与第三方组件声明装在本应用内：
          <code>Sextant.app/Contents/Resources/licenses/</code>
          （在访达里右键本应用 →「显示包内容」即可看到）。
        </p>

        <h3>其它组件</h3>
        <ul>
          <li>Tauri —— MIT / Apache-2.0</li>
          <li>Vue —— MIT</li>
          <li>Rust regex 引擎 —— MIT / Apache-2.0</li>
          <li>unicode-normalization —— MIT / Apache-2.0 与 Unicode-3.0</li>
          <li>notify（FSEvents 封装）—— CC0-1.0 / MIT</li>
        </ul>
        <p class="credits-note">
          各第三方组件的完整许可清单见
          <code>THIRD-PARTY-NOTICES.md</code>，与应用一起打包。
        </p>

        <h3>隐私</h3>
        <p>
          本软件<strong>不联网</strong>：没有遥测、没有崩溃上报、没有更新检查。
          索引只保存在你自己的机器上。
        </p>
      </div>
    </section>
  </div>
</template>
