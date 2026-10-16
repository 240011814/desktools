/* 轻量 Markdown 渲染（仅用于剪贴板内容的「渲染预览」，纯函数便于单元测试）
 *
 * 安全约定：先把原文整体转义 HTML，再替换为白名单标签；链接只生成 data-url 属性，
 * 由组件拦截点击后走 opener 插件打开，不产生 <a href> 导航 / javascript: 注入。
 */

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** 行内元素：代码 / 链接先抽成占位符，避免其中的 * _ 等被后续规则误处理 */
function renderInline(escaped: string): string {
  const stashed: string[] = [];
  const stash = (html: string): string => {
    stashed.push(html);
    return `\u0000${stashed.length - 1}\u0000`;
  };

  let s = escaped;
  s = s.replace(/`([^`\n]+)`/g, (_m, code: string) => stash(`<code>${code}</code>`));
  s = s.replace(
    /!?\[([^\]\n]*)\]\(([^()\s]+)\)/g,
    (_m, label: string, url: string) => {
      if (!/^(https?:\/\/|mailto:)/i.test(url)) return `${label}（${url}）`;
      return stash(`<a class="md-link" data-url="${url}">${label || url}</a>`);
    },
  );
  s = s.replace(/(\*\*|__)(?=\S)([\s\S]*?\S)\1/g, "<strong>$2</strong>");
  s = s.replace(/(^|[^A-Za-z0-9*])_(?=\S)([^_\n]*?\S)_(?![A-Za-z0-9_])/g, "$1<em>$2</em>");
  s = s.replace(/\*(?=\S)([^*\n]*?\S)\*/g, "<em>$1</em>");
  s = s.replace(/~~(?=\S)([\s\S]*?\S)~~/g, "<del>$1</del>");
  s = s.replace(/\u0000(\d+)\u0000/g, (_m, n: string) => stashed[Number(n)] ?? "");
  return s;
}

function splitTableRow(line: string): string[] {
  return line
    .trim()
    .replace(/^\||\|$/g, "")
    .split("|")
    .map((cell) => cell.trim());
}

export function renderMarkdown(source: string): string {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  const para: string[] = [];
  let i = 0;

  const flushParagraph = () => {
    if (!para.length) return;
    out.push(`<p>${renderInline(escapeHtml(para.join(" ")))}</p>`);
    para.length = 0;
  };

  while (i < lines.length) {
    const line = lines[i];

    // 代码块（不支持嵌套）
    if (/^```/.test(line)) {
      flushParagraph();
      const buf: string[] = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        buf.push(lines[i]);
        i++;
      }
      i++;
      out.push(`<pre class="md-pre"><code>${escapeHtml(buf.join("\n"))}</code></pre>`);
      continue;
    }

    if (!line.trim()) {
      flushParagraph();
      i++;
      continue;
    }

    if (/^\s{0,3}(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      flushParagraph();
      out.push("<hr />");
      i++;
      continue;
    }

    const heading = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (heading) {
      flushParagraph();
      const level = heading[1].length;
      out.push(`<h${level}>${renderInline(escapeHtml(heading[2]))}</h${level}>`);
      i++;
      continue;
    }

    if (/^\s{0,3}>\s?/.test(line)) {
      flushParagraph();
      const buf: string[] = [];
      while (i < lines.length && /^\s{0,3}>\s?/.test(lines[i])) {
        buf.push(lines[i].replace(/^\s{0,3}>\s?/, ""));
        i++;
      }
      out.push(`<blockquote>${renderInline(escapeHtml(buf.join(" ")))}</blockquote>`);
      continue;
    }

    // 表格：| a | b | 后面跟随 |---|---|
    if (
      /^\s*\|.*\|\s*$/.test(line) &&
      i + 1 < lines.length &&
      /^\s*\|?[\s:|-]*-[\s:|-]*\|?\s*$/.test(lines[i + 1])
    ) {
      flushParagraph();
      const headers = splitTableRow(line);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) {
        rows.push(splitTableRow(lines[i]));
        i++;
      }
      const head = headers.map((h) => `<th>${renderInline(escapeHtml(h))}</th>`).join("");
      const body = rows
        .map((row) => `<tr>${row.map((c) => `<td>${renderInline(escapeHtml(c))}</td>`).join("")}</tr>`)
        .join("");
      out.push(`<table><thead><tr>${head}</tr></thead><tbody>${body}</tbody></table>`);
      continue;
    }

    // 列表（不支持嵌套）
    if (/^\s*([-*+]|\d+[.)])\s+\S/.test(line)) {
      flushParagraph();
      const ordered = /^\s*\d/.test(line);
      const items: string[] = [];
      while (i < lines.length) {
        const m = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(lines[i]);
        if (!m) break;
        items.push(m[2]);
        i++;
      }
      const tag = ordered ? "ol" : "ul";
      out.push(`<${tag}>${items.map((it) => `<li>${renderInline(escapeHtml(it))}</li>`).join("")}</${tag}>`);
      continue;
    }

    para.push(line);
    i++;
  }

  flushParagraph();
  return out.join("\n");
}
