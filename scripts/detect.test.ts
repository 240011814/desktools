/* 剪贴板内容识别 / 数学算式 / Markdown 渲染的单元测试
 *
 * 运行：pnpm test:unit（Node 24 原生 TypeScript 类型剥离 + 内置 test runner，无需额外依赖）
 */
import test from "node:test";
import assert from "node:assert/strict";
import { detectContent, evaluateMath, looksLikeMarkdown } from "../src/detect.ts";
import { renderMarkdown } from "../src/markdown.ts";

/* ---------- 链接 ---------- */

test("url: http/https 链接", () => {
  const info = detectContent("https://github.com/hyy/desktools");
  assert.equal(info.kind, "url");
  assert.equal(info.label, "链接");
  assert.equal(info.link, "https://github.com/hyy/desktools");
});

test("url: www 开头自动补 https", () => {
  const info = detectContent("www.example.com");
  assert.equal(info.kind, "url");
  assert.equal(info.link, "https://www.example.com");
});

test("url: 常见顶级域名的裸域名", () => {
  const info = detectContent("example.com/path?q=1");
  assert.equal(info.kind, "url");
  assert.equal(info.link, "https://example.com/path?q=1");
});

test("url: 去掉句末中文标点", () => {
  const info = detectContent("https://example.com/foo。");
  assert.equal(info.kind, "url");
  assert.equal(info.link, "https://example.com/foo");
});

test("url: 成对括号属于链接", () => {
  const info = detectContent("https://en.wikipedia.org/wiki/Foo_(bar)");
  assert.equal(info.kind, "url");
  assert.equal(info.link, "https://en.wikipedia.org/wiki/Foo_(bar)");
});

test("url: 文件名 / 版本号不误判", () => {
  assert.equal(detectContent("Node.js").kind, "text");
  assert.equal(detectContent("README.md").kind, "text");
  assert.equal(detectContent("tauri.conf.json").kind, "text");
});

/* ---------- 邮箱 / 电话 ---------- */

test("email: 地址与 mailto:", () => {
  const a = detectContent("user@example.com");
  assert.equal(a.kind, "email");
  assert.equal(a.link, "mailto:user@example.com");
  const b = detectContent("mailto:user@example.com");
  assert.equal(b.kind, "email");
  assert.equal(b.link, "mailto:user@example.com");
});

test("phone: 中国大陆手机号 / 带国际区号 / 座机", () => {
  const mobile = detectContent("13800138000");
  assert.equal(mobile.kind, "phone");
  assert.equal(mobile.link, "tel:13800138000");
  const intl = detectContent("+86 138 0013 8000");
  assert.equal(intl.kind, "phone");
  assert.equal(intl.link, "tel:+8613800138000");
  const landline = detectContent("010-12345678");
  assert.equal(landline.kind, "phone");
});

test("phone: 短号码 / 日期不算电话", () => {
  assert.equal(detectContent("10086").kind, "text");
  assert.equal(detectContent("2023-01-01").kind, "text");
});

/* ---------- 颜色 ---------- */

test("color: HEX / RGB / HSL 归一化", () => {
  const hex = detectContent("#4b7bec");
  assert.equal(hex.kind, "color");
  assert.equal(hex.color?.hex, "#4b7bec");
  assert.equal(hex.color?.rgb, "rgb(75, 123, 236)");
  assert.equal(hex.color?.hsl, "hsl(222, 81%, 61%)");

  const short = detectContent("#fff");
  assert.equal(short.kind, "color");
  assert.equal(short.color?.hex, "#ffffff");

  const rgb = detectContent("rgb(75, 123, 236)");
  assert.equal(rgb.kind, "color");
  assert.equal(rgb.color?.hex, "#4b7bec");

  const hsl = detectContent("hsl(222, 81%, 61%)");
  assert.equal(hsl.kind, "color");
  assert.equal(hsl.color?.hex, "#4b7bec");
});

test("color: 越界数值不识别", () => {
  assert.equal(detectContent("rgb(300, 0, 0)").kind, "text");
  assert.equal(detectContent("hsl(200, 150%, 50%)").kind, "text");
});

/* ---------- JSON ---------- */

test("json: 对象 / 数组（含多行）", () => {
  const obj = detectContent('{"a": 1, "b": [2, 3]}');
  assert.equal(obj.kind, "json");
  assert.equal(obj.json?.min, '{"a":1,"b":[2,3]}');
  assert.ok(obj.json?.pretty.includes('\n  "a": 1'));

  const multiline = detectContent('{\n  "a": 1\n}');
  assert.equal(multiline.kind, "json");
});

test("json: 字符串 / 数字 / 非法 JSON 不算", () => {
  assert.equal(detectContent('"hello"').kind, "text");
  assert.equal(detectContent("123").kind, "text");
  assert.equal(detectContent('{"a": 1').kind, "text");
});

/* ---------- 路径 ---------- */

test("path: macOS / Linux 绝对路径", () => {
  const info = detectContent("/Users/hyy/Documents/note.txt");
  assert.equal(info.kind, "path");
  assert.equal(info.path, "/Users/hyy/Documents/note.txt");
  assert.equal(detectContent("/etc/hosts").kind, "path");
  // 单段绝对路径太容易误判，只有带扩展名才识别
  assert.equal(detectContent("/usr").kind, "text");
});

test("path: Home 目录与 file:// 链接", () => {
  const home = detectContent("~/Desktop/file.md");
  assert.equal(home.kind, "path");
  assert.equal(home.path, "~/Desktop/file.md");

  const fileUrl = detectContent("file:///Users/hyy/a%20b.txt");
  assert.equal(fileUrl.kind, "path");
  assert.equal(fileUrl.path, "/Users/hyy/a b.txt");
});

test("path: Windows 盘符 / UNC / file:///C:/", () => {
  const win = detectContent("C:\\Users\\hyy\\note.txt");
  assert.equal(win.kind, "path");
  assert.equal(detectContent("C:/Users/hyy/note.txt").kind, "path");
  const unc = detectContent("\\\\server\\share\\file.txt");
  assert.equal(unc.kind, "path");
  const fileUrl = detectContent("file:///C:/Users/hyy/note.txt");
  assert.equal(fileUrl.kind, "path");
  assert.equal(fileUrl.path, "C:/Users/hyy/note.txt");
});

test("path: 句子不误判为路径", () => {
  assert.equal(detectContent("/usr/local/bin is where it is").kind, "text");
  assert.equal(detectContent("/tmp 下的临时文件，请勿删除。").kind, "text");
});

/* ---------- Base64 ---------- */

test("base64: ASCII / 中文解码", () => {
  const ascii = detectContent("aGVsbG8=");
  assert.equal(ascii.kind, "base64");
  assert.equal(ascii.decoded, "hello");

  const zh = detectContent("5L2g5aW977yM5LiW55WM");
  assert.equal(zh.kind, "base64");
  assert.equal(zh.decoded, "你好，世界");
});

test("base64: 普通单词 / 哈希不误判", () => {
  assert.equal(detectContent("hello").kind, "text");
  assert.equal(detectContent("test").kind, "text");
  assert.equal(detectContent("da39a3ee5e6b4b0d3255bfef95601890afd80709").kind, "text");
});

/* ---------- 数学算式 ---------- */

test("math: 四则运算 / 优先级 / 括号 / 幂 / 取模", () => {
  assert.equal(evaluateMath("1+2*3"), "7");
  assert.equal(evaluateMath("(1+2)*3"), "9");
  assert.equal(evaluateMath("2^10"), "1024");
  assert.equal(evaluateMath("10%3"), "1");
  assert.equal(evaluateMath("0.1+0.2"), "0.3");
  assert.equal(evaluateMath("-2^2"), "-4");
  assert.equal(detectContent("3*(4+5)").kind, "math");
  assert.equal(detectContent("3*(4+5)").result, "27");
});

test("math: 中文输入习惯 × ÷ − 与全角括号", () => {
  assert.equal(evaluateMath("6 × 7"), "42");
  assert.equal(evaluateMath("8 ÷ 2"), "4");
  assert.equal(evaluateMath("（1+2）×3"), "9");
});

test("math: 非法 / 危险输入返回 null", () => {
  assert.equal(evaluateMath("1/0"), null);
  assert.equal(evaluateMath("1..2"), null);
  assert.equal(evaluateMath("hello"), null);
  assert.equal(evaluateMath("1+"), null);
  assert.equal(evaluateMath("alert(1)"), null);
  assert.equal(evaluateMath("50%"), null);
  assert.equal(evaluateMath("2023-01-01"), null);
  assert.equal(evaluateMath("2023-01"), null);
});

/* ---------- Markdown ---------- */

test("markdown: 判定信号", () => {
  assert.ok(looksLikeMarkdown("# 标题\n正文"));
  assert.ok(looksLikeMarkdown("```js\nconsole.log(1)\n```"));
  assert.ok(looksLikeMarkdown("**加粗**"));
  assert.ok(looksLikeMarkdown("> 引用"));
  assert.ok(looksLikeMarkdown("- 一\n- 二"));
  assert.ok(!looksLikeMarkdown("普通的一句话。"));
  assert.ok(!looksLikeMarkdown("第一行\n第二行"));
});

test("markdown: 渲染标题 / 列表 / 加粗", () => {
  const html = renderMarkdown("# Hi\n\n- a\n- b\n\n**bold**");
  assert.ok(html.includes("<h1>Hi</h1>"));
  assert.ok(html.includes("<ul><li>a</li><li>b</li></ul>"));
  assert.ok(html.includes("<strong>bold</strong>"));
});

test("markdown: 转义 HTML，不产生注入", () => {
  const html = renderMarkdown("<script>alert(1)</script>");
  assert.ok(!html.includes("<script>"));
  assert.ok(html.includes("&lt;script&gt;"));
});

test("markdown: 链接生成 data-url，非法协议不生成链接", () => {
  const ok = renderMarkdown("[x](https://example.com)");
  assert.ok(ok.includes('data-url="https://example.com"'));
  const bad = renderMarkdown("[x](javascript:alert(1))");
  assert.ok(!bad.includes("<a "));
});

test("markdown: 行内代码内容保持转义", () => {
  const html = renderMarkdown("`a<b`");
  assert.ok(html.includes("<code>a&lt;b</code>"));
});

/* ---------- 纯文本兜底 ---------- */

test("text: 普通文本不做任何处理", () => {
  const info = detectContent("今天天气不错，适合写代码。");
  assert.equal(info.kind, "text");
  assert.equal(info.label, "");
  assert.equal(info.link, undefined);
});
