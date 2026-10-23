import { describe, expect, it } from "vitest";
import { segmentize } from "./highlight";

describe("segmentize", () => {
  it("无命中时整段是普通文本", () => {
    expect(segmentize("hello", [])).toEqual([{ text: "hello", hit: false }]);
  });

  it("普通命中切成两段", () => {
    expect(segmentize("hello", [{ start: 1, length: 3 }])).toEqual([
      { text: "h", hit: false },
      { text: "ell", hit: true },
      { text: "o", hit: false },
    ]);
  });

  it("G_MAXUINT 长度的区间按字符串长度截断", () => {
    // 引擎用 end_index == G_MAXUINT 表示「高亮到结尾」，
    // 于是 length = G_MAXUINT - start（实测 ext:java 命中 A.java 时 length=4294967293）
    expect(segmentize("A.java", [{ start: 2, length: 4294967293 }])).toEqual([
      { text: "A.", hit: false },
      { text: "java", hit: true },
    ]);
  });

  it("重叠区间合并", () => {
    expect(segmentize("abcdef", [
      { start: 1, length: 3 },
      { start: 2, length: 3 },
    ])).toEqual([
      { text: "a", hit: false },
      { text: "bcde", hit: true },
      { text: "f", hit: false },
    ]);
  });
});
