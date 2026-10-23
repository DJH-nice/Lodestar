import { describe, expect, it } from "vitest";
import { tokenize } from "./tokenizer";
import type { SyntaxTables } from "./types";

/* 这三张表与 shim 的真实内容一致（C 探针实测打印过）：
   MODIFIERS: nocase,exact,file,files,folder,folders,path,nopath,regex,noregex,
              case,nofileonly,nofilesonly,nofolderonly,nofoldersonly
   FIELDS:     childcount,childfilecount,childfoldercount,contenttype,depth,dm,
              datemodified,empty,ext,parent,parents,size,app,archive,audio,doc,pic,video
   OPERATORS:  NOT,AND,&&,OR,||,(,),=,:,<,<=,>,>=,!,..
   应用运行时用的是引擎自己的表（通过 syntax_tables 命令拿），
   这里写死是为了让测试不依赖后端。 */
const tables: SyntaxTables = {
  modifiers: [
    "nocase", "exact", "file", "files", "folder", "folders",
    "path", "nopath", "regex", "noregex", "case",
    "nofileonly", "nofilesonly", "nofolderonly", "nofoldersonly",
  ],
  fields: [
    "childcount", "childfilecount", "childfoldercount", "contenttype",
    "depth", "dm", "datemodified", "empty", "ext", "parent", "parents", "size",
    "app", "archive", "audio", "doc", "pic", "video",
  ],
  operators: [
    "NOT", "AND", "&&", "OR", "||", "(", ")", "=", ":",
    "<", "<=", ">", ">=", "!", "..",
  ],
};

describe("tokenize", () => {
  it("识别修饰符", () => {
    const t = tokenize("path:foo", tables);
    expect(t[0]).toEqual({ text: "path", kind: "modifier" });
  });

  it("识别函数字段", () => {
    const t = tokenize("ext:ts", tables);
    expect(t[0]).toEqual({ text: "ext", kind: "field" });
  });

  it("识别 NOT 关键字与 !", () => {
    expect(tokenize("NOT foo", tables)[0].kind).toBe("not");
    expect(tokenize("!foo", tables)[0].kind).toBe("not");
  });

  it("引号短语整体成 token", () => {
    const t = tokenize('"多 词"', tables);
    expect(t).toEqual([{ text: '"多 词"', kind: "string" }]);
  });

  it("引号里的冒号不被当成字段分隔", () => {
    const t = tokenize('"a:b"', tables);
    expect(t).toEqual([{ text: '"a:b"', kind: "string" }]);
  });

  it("未知字段不标成 field（由 shim 的 warning 负责提示）", () => {
    const t = tokenize("name:foo", tables);
    expect(t[0].kind).toBe("plain");
  });

  it("路径整词标成 path —— 不是正则", () => {
    // 上游没有 "/.../" 正则语法，正则靠 regex: 修饰符开启
    const t = tokenize("/usr/local", tables);
    expect(t).toEqual([{ text: "/usr/local", kind: "path" }]);
  });

  it("比较符标成 operator", () => {
    const t = tokenize("size:>10M", tables);
    expect(t.map((x) => x.kind)).toEqual(["field", "plain", "operator", "plain"]);
  });
});
