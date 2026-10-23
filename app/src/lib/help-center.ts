/**
 * 帮助中心的三个页签。
 *
 *  【为什么速查和手册放一个面板里】
 *  速查是**照着抄**的写法清单（按「我想找什么」组织），
 *  手册是**愿意读**的完整文档。两者内容不同、用途不同，
 *  但对用户来说它们是同一件事的两个深度 ——
 *  放在一个面板里，用户不用先判断「我现在该看哪个」。
 */
export type HelpTab = "cheatsheet" | "syntax" | "manual";

export interface HelpTabInfo {
  key: HelpTab;
  /** 页签上的字。 */
  label: string;
  /** 页签的 title（鼠标悬停时的那行说明）。 */
  hint: string;
}

/** 页签的顺序与文案。 */
export const helpTabs: HelpTabInfo[] = [
  {
    key: "cheatsheet",
    label: "语法速查",
    hint: "按「我想找什么」列的写法清单 —— 点一下就能填进搜索框",
  },
  {
    key: "syntax",
    label: "完整语法表",
    hint: "引擎支持的每一个字段、修饰符、运算符",
  },
  {
    key: "manual",
    label: "完整手册",
    hint: "整本使用手册（索引、书签、快捷键、常见问题）",
  },
];
