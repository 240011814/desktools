/* 剪贴板内容智能识别（纯函数，便于单元测试：scripts/detect.test.ts）
 *
 * 识别全部在本机离线完成，识别失败统一归为「纯文本」，前端不做任何特殊处理。
 * 单行文本顺序：链接 → 邮箱 → 电话 → 颜色 → JSON → 路径 → Base64 → 算式 → Markdown
 * 多行文本顺序：JSON → Markdown → Base64
 */

export type ContentKind =
  | "url"
  | "email"
  | "phone"
  | "color"
  | "json"
  | "path"
  | "base64"
  | "math"
  | "markdown"
  | "text";

export interface ColorInfo {
  /** 归一化后的 #rrggbb */
  hex: string;
  /** rgb(r, g, b) */
  rgb: string;
  /** hsl(h, s%, l%) */
  hsl: string;
}

export interface ContentInfo {
  kind: ContentKind;
  /** 徽章中文标签（纯文本为空串） */
  label: string;
  /** 可打开链接：kind=url 为浏览器链接；kind=email 为 mailto:；kind=phone 为 tel: */
  link?: string;
  /** kind=color 的颜色信息 */
  color?: ColorInfo;
  /** kind=json 的格式化与压缩结果 */
  json?: { pretty: string; min: string };
  /** kind=base64 的解码文本 */
  decoded?: string;
  /** kind=math 的计算结果（已格式化） */
  result?: string;
  /** kind=path 的文件 / 文件夹路径（file:// 已还原、引号已去除、~ 保留待前端展开） */
  path?: string;
}

const LABELS: Record<Exclude<ContentKind, "text">, string> = {
  url: "链接",
  email: "邮箱",
  phone: "电话",
  color: "颜色",
  json: "JSON",
  path: "路径",
  base64: "Base64",
  math: "算式",
  markdown: "Markdown",
};

const TEXT: ContentInfo = { kind: "text", label: "" };

/* ---------- 链接 ---------- */

/** 裸域名（无协议）只在常见顶级域名时识别，避免 config.json / Node.js 之类误判 */
const BARE_DOMAIN_TLDS = new Set([
  "com", "cn", "org", "net", "io", "dev", "app", "ai", "co", "me",
  "info", "xyz", "top", "edu", "gov", "tech", "site", "online", "store",
  "blog", "cc", "tv", "uk", "de", "fr", "jp", "kr", "ru", "in", "us",
  "ca", "au", "hk", "tw", "sg", "eu", "ch", "it", "es", "nl", "se",
  "no", "fi", "pl", "br", "mx", "pro", "live", "cloud", "design", "art",
  "sh", "so", "gg", "ly", "be", "im", "id", "one", "page", "run", "link",
]);

function countChar(s: string, ch: string): number {
  let n = 0;
  for (const c of s) if (c === ch) n++;
  return n;
}

/** 去掉 URL 末尾误带的标点（句号 / 中文句号 / 不成对的右括号等） */
function trimUrlPunctuation(s: string): string {
  for (;;) {
    const next = s.replace(/[.,;:!?'"、。，；：！？”’》】]+$/, "");
    if (next.length !== s.length) {
      s = next;
      continue;
    }
    const last = s[s.length - 1];
    if (last === ")" || last === "）" || last === "]") {
      const open = last === "）" ? "（" : last === "]" ? "[" : "(";
      if (countChar(s, last) > countChar(s, open)) {
        s = s.slice(0, -1);
        continue;
      }
    }
    return s;
  }
}

function detectUrl(t: string): string | null {
  let candidate: string;
  if (/^https?:\/\//i.test(t)) {
    candidate = t;
  } else if (/^www\.[^\s/]+/i.test(t)) {
    candidate = `https://${t}`;
  } else if (/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+(\/[^\s]*)?$/i.test(t)) {
    const host = t.split("/")[0];
    const tld = host.split(".").pop()!.toLowerCase();
    if (!BARE_DOMAIN_TLDS.has(tld)) return null;
    candidate = `https://${t}`;
  } else {
    return null;
  }

  candidate = trimUrlPunctuation(candidate);
  if (!candidate || /\s/.test(candidate)) return null;
  try {
    const url = new URL(candidate);
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    if (!url.hostname.includes(".") && url.hostname !== "localhost") return null;
    return candidate;
  } catch {
    return null;
  }
}

/* ---------- 邮箱 / 电话 ---------- */

const EMAIL_RE = /^[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$/;

function detectEmail(t: string): string | null {
  const mailto = /^mailto:([^\s?]+)$/i.exec(t);
  if (mailto && EMAIL_RE.test(mailto[1])) return mailto[1];
  if (EMAIL_RE.test(t)) return t;
  return null;
}

function detectPhone(t: string): string | null {
  // 日期（2023-01-01）等数字组合不当作电话
  if (/^\d{4}-\d{1,2}-\d{1,2}$/.test(t)) return null;
  const raw = /^tel:/i.test(t) ? t.replace(/^tel:/i, "") : t;
  if (!/^\+?[\d\s()-]{6,20}$/.test(raw)) return null;
  const digits = raw.replace(/\D/g, "");
  if (digits.length < 7 || digits.length > 15) return null;
  const isCnMobile = /^1[3-9]\d{9}$/.test(digits);
  const structured = raw.trim().startsWith("+") || /[\s()-]/.test(raw);
  if (!isCnMobile && !structured) return null;
  return raw.trim().startsWith("+") ? `+${digits}` : digits;
}

/* ---------- 颜色 ---------- */

const HEX_RE = /^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const RGB_RE = /^rgba?\(\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*[, ]\s*(\d{1,3})\s*(?:[,/]\s*[\d.]+%?\s*)?\)$/i;
const HSL_RE = /^hsla?\(\s*(\d{1,3})(?:deg)?\s*[, ]\s*([\d.]+)%\s*[, ]\s*([\d.]+)%\s*(?:[,/]\s*[\d.]+%?\s*)?\)$/i;

function clamp255(n: number): number {
  return Math.max(0, Math.min(255, Math.round(n)));
}

function rgbToHslString(r: number, g: number, b: number): string {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  const d = max - min;
  let h = 0;
  let s = 0;
  if (d !== 0) {
    s = d / (1 - Math.abs(2 * l - 1));
    if (max === rn) h = ((gn - bn) / d + 6) % 6;
    else if (max === gn) h = (bn - rn) / d + 2;
    else h = (rn - gn) / d + 4;
    h *= 60;
  }
  return `hsl(${Math.round(h)}, ${Math.round(s * 100)}%, ${Math.round(l * 100)}%)`;
}

function toColorInfo(r: number, g: number, b: number): ColorInfo {
  const rr = clamp255(r);
  const gg = clamp255(g);
  const bb = clamp255(b);
  const hex = `#${[rr, gg, bb].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
  return { hex, rgb: `rgb(${rr}, ${gg}, ${bb})`, hsl: rgbToHslString(rr, gg, bb) };
}

function hslToRgb(h: number, s: number, l: number): { r: number; g: number; b: number } {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = l - c / 2;
  let r = 0;
  let g = 0;
  let b = 0;
  if (h < 60) [r, g, b] = [c, x, 0];
  else if (h < 120) [r, g, b] = [x, c, 0];
  else if (h < 180) [r, g, b] = [0, c, x];
  else if (h < 240) [r, g, b] = [0, x, c];
  else if (h < 300) [r, g, b] = [x, 0, c];
  else [r, g, b] = [c, 0, x];
  return { r: (r + m) * 255, g: (g + m) * 255, b: (b + m) * 255 };
}

function detectColor(t: string): ColorInfo | null {
  const hex = HEX_RE.exec(t);
  if (hex) {
    let h = hex[1];
    if (h.length === 3) h = h.split("").map((c) => c + c).join("");
    else if (h.length === 4) h = h.slice(0, 3).split("").map((c) => c + c).join(""); // #rgba 去掉 alpha
    else if (h.length === 8) h = h.slice(0, 6);
    return toColorInfo(parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16));
  }

  const rgb = RGB_RE.exec(t);
  if (rgb) {
    const [r, g, b] = [Number(rgb[1]), Number(rgb[2]), Number(rgb[3])];
    if (r > 255 || g > 255 || b > 255) return null;
    return toColorInfo(r, g, b);
  }

  const hsl = HSL_RE.exec(t);
  if (hsl) {
    const h = Number(hsl[1]);
    const s = Number(hsl[2]);
    const l = Number(hsl[3]);
    if (s > 100 || l > 100) return null;
    const { r, g, b } = hslToRgb(h % 360, s / 100, l / 100);
    return toColorInfo(r, g, b);
  }

  return null;
}

/* ---------- JSON ---------- */

function detectJson(t: string): { pretty: string; min: string } | null {
  const trimmed = t.trim();
  if (!/^[[{]/.test(trimmed)) return null;
  try {
    const value: unknown = JSON.parse(trimmed);
    if (typeof value !== "object" || value === null) return null;
    return { pretty: JSON.stringify(value, null, 2), min: JSON.stringify(value) };
  } catch {
    return null;
  }
}

/* ---------- 文件 / 文件夹路径 ---------- */

function detectPath(t: string): string | null {
  const s = t.trim().replace(/^["'「『]|["'」』]$/g, "");
  if (s.length < 2 || s.length > 1024) return null;
  // 句末标点 / 全角标点大概率是普通文本而非路径
  if (/[，。；：！？]/.test(s)) return null;

  if (/^file:\/\//i.test(s)) {
    try {
      const url = new URL(s);
      if (url.host && url.host !== "localhost") return null;
      let p = decodeURIComponent(url.pathname);
      if (/^\/[A-Za-z]:[\\/]/.test(p)) p = p.slice(1); // file:///C:/... → C:/...
      return p || null;
    } catch {
      return null;
    }
  }

  if (/^[A-Za-z]:[\\/]/.test(s) || /^\\\\[^\\]+\\/.test(s)) return s; // Windows 盘符 / UNC
  if (s === "~" || s.startsWith("~/")) return s;

  if (s.startsWith("/") && !s.startsWith("//")) {
    // 空格过多的多半是一句话（如 "/usr/local/bin is where it is"），按文本处理
    if (countChar(s, " ") > 3) return null;
    const segments = s.split("/").filter(Boolean);
    if (segments.length >= 2) return s;
    if (segments.length === 1 && /\.[A-Za-z0-9]{1,12}$/.test(segments[0])) return s;
  }
  return null;
}

/* ---------- Base64 ---------- */

function detectBase64(t: string): string | null {
  const compact = t.replace(/\s+/g, "");
  if (compact.length < 8 || compact.length > 100_000) return null;
  if (!/^[A-Za-z0-9+/_-]+={0,2}$/.test(compact)) return null;
  // 带填充时长度必须已对齐，否则补不齐 = 不是合法 Base64
  if (/=$/.test(compact) && compact.length % 4 !== 0) return null;

  const std = compact.replace(/-/g, "+").replace(/_/g, "/");
  const padded = std + "=".repeat((4 - (std.length % 4)) % 4);
  if (padded.length % 4 !== 0) return null;

  let decoded: string;
  try {
    const bin = atob(padded);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    decoded = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }

  if (!decoded || decoded.length > 200_000) return null;
  // 解码结果必须是可读文本（含字母 / 数字），且不含控制字符
  if (!/[\p{L}\p{N}]/u.test(decoded)) return null;
  if (/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/.test(decoded)) return null;
  return decoded;
}

/* ---------- 数学算式 ---------- */

function formatMathResult(v: number): string {
  if (Number.isInteger(v)) {
    return Math.abs(v) < 1e15 ? String(v) : v.toPrecision(12);
  }
  return String(Number.parseFloat(v.toPrecision(12)));
}

/**
 * 计算简单数学算式（+ - * / % ^ 与括号），失败返回 null。
 * 用自写递归下降解析器而非 eval / new Function，避免代码注入风险。
 * 支持 × ÷ − 与全角括号的中文输入习惯；^ 为幂运算（右结合）。
 */
export function evaluateMath(input: string): string | null {
  const src = input
    .trim()
    .replace(/^=\s*/, "")
    .replace(/×/g, "*")
    .replace(/÷/g, "/")
    .replace(/−/g, "-")
    .replace(/（/g, "(")
    .replace(/）/g, ")");
  if (!src || src.length > 100) return null;
  if (!/^[\d\s+\-*/%^().]+$/.test(src)) return null;
  if (!/\d/.test(src) || !/[+\-*/%^]/.test(src)) return null;
  if (/^\d{4}-\d{1,2}(-\d{1,2})?$/.test(src)) return null; // 日期 / 年月不是算式

  const tokens: string[] = [];
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    if (/[\d.]/.test(c)) {
      let j = i;
      while (j < src.length && /[\d.]/.test(src[j])) j++;
      const num = src.slice(i, j);
      if (!/^\d+(\.\d+)?$/.test(num)) return null; // 拒绝 1.2.3 这类
      tokens.push(num);
      i = j;
      continue;
    }
    if ("+-*/%^()".includes(c)) {
      tokens.push(c);
      i++;
      continue;
    }
    return null;
  }

  let pos = 0;
  const peek = (): string => (pos < tokens.length ? tokens[pos] : "");

  function primary(): number {
    const token = peek();
    if (token === "(") {
      pos++;
      const v = expression();
      if (peek() !== ")") throw new Error("括号不匹配");
      pos++;
      return v;
    }
    if (!token || !/^\d/.test(token)) throw new Error("非法表达式");
    pos++;
    return Number(token);
  }

  function power(): number {
    const base = primary();
    if (peek() === "^") {
      pos++;
      return Math.pow(base, unary());
    }
    return base;
  }

  function unary(): number {
    const token = peek();
    if (token === "-") {
      pos++;
      return -unary();
    }
    if (token === "+") {
      pos++;
      return unary();
    }
    return power();
  }

  function term(): number {
    let v = unary();
    while (peek() === "*" || peek() === "/" || peek() === "%") {
      const op = tokens[pos++];
      const r = unary();
      if (op === "*") v *= r;
      else if (op === "/") v /= r;
      else v %= r;
    }
    return v;
  }

  function expression(): number {
    let v = term();
    while (peek() === "+" || peek() === "-") {
      const op = tokens[pos++];
      const r = term();
      v = op === "+" ? v + r : v - r;
    }
    return v;
  }

  try {
    const value = expression();
    if (pos !== tokens.length || !Number.isFinite(value)) return null;
    return formatMathResult(value);
  } catch {
    return null;
  }
}

/* ---------- Markdown ---------- */

/** 判断是否像 Markdown（宁可漏判，别把普通文本标成 Markdown） */
export function looksLikeMarkdown(text: string): boolean {
  if (text.length < 3) return false;
  const strong = [
    /^```/m,
    /^#{1,6}\s+\S/m,
    /^>\s+\S/m,
    /!?\[[^\]\n]*\]\([^()\s]+\)/,
    /(\*\*|__)[^\n]+?\1/,
    /`[^`\n]+`/,
    /^\|.+\|$/m,
    /^\s{0,3}(-{3,}|\*{3,}|_{3,})\s*$/m,
  ];
  if (strong.some((re) => re.test(text))) return true;
  const listLines = text.split("\n").filter((line) => /^\s*([-*+]|\d+[.)])\s+\S/.test(line)).length;
  return listLines >= 2;
}

/* ---------- 统一入口 ---------- */

export function detectContent(raw: string): ContentInfo {
  const t = raw.trim();
  if (!t) return TEXT;

  if (!/[\r\n]/.test(t)) {
    const url = detectUrl(t);
    if (url) return { kind: "url", label: LABELS.url, link: url };
    const email = detectEmail(t);
    if (email) return { kind: "email", label: LABELS.email, link: `mailto:${email}` };
    const phone = detectPhone(t);
    if (phone) return { kind: "phone", label: LABELS.phone, link: `tel:${phone}` };
    const color = detectColor(t);
    if (color) return { kind: "color", label: LABELS.color, color };
    const json = detectJson(t);
    if (json) return { kind: "json", label: LABELS.json, json };
    const path = detectPath(t);
    if (path) return { kind: "path", label: LABELS.path, path };
    const decoded = detectBase64(t);
    if (decoded) return { kind: "base64", label: LABELS.base64, decoded };
    const result = evaluateMath(t);
    if (result) return { kind: "math", label: LABELS.math, result };
    if (looksLikeMarkdown(t)) return { kind: "markdown", label: LABELS.markdown };
    return TEXT;
  }

  const json = detectJson(t);
  if (json) return { kind: "json", label: LABELS.json, json };
  if (looksLikeMarkdown(t)) return { kind: "markdown", label: LABELS.markdown };
  const decoded = detectBase64(t);
  if (decoded) return { kind: "base64", label: LABELS.base64, decoded };
  return TEXT;
}
