import { LEXER_WORDS } from "./definitions";

export interface SpanOnLine {
  start: number;
  length: number;
}

export interface ScannedLine {
  types: SpanOnLine[];
  keywords: SpanOnLine[];
  effects: SpanOnLine[];
}

const BUILTIN_TYPES = [
  "Tensor",
  "List",
  "Dict",
  "Int",
  "Float",
  "Bool",
  "String",
  "Bytes",
  "Unit",
  "BF16",
  "F16",
  "F32",
  "I8",
] as const;

const KEYWORD_EXTRA = ["method", "static", "init"] as const;
const EFFECTS = ["alloc", "io"] as const;

const KEYWORD_RE = new RegExp(
  `\\b(?:${[...LEXER_WORDS, ...KEYWORD_EXTRA].join("|")})\\b`,
  "g",
);
const BUILTIN_TYPE_RE = new RegExp(`\\b(?:${BUILTIN_TYPES.join("|")})\\b`, "g");
const EFFECT_RE = new RegExp(`\\b(?:${EFFECTS.join("|")})\\b`, "g");
const TYPE_NAME_RE = /\b[A-Z][A-Za-z0-9_]*\b/g;
const BUILTIN_TYPE_SET = new Set<string>(BUILTIN_TYPES);

export function scanSalLine(line: string): ScannedLine {
  const code = cutComment(line);
  const skip = opaqueRanges(code);
  const types: SpanOnLine[] = [];
  const keywords: SpanOnLine[] = [];
  const effects: SpanOnLine[] = [];
  collectRegex(code, BUILTIN_TYPE_RE, skip, types);
  collectTypeNames(code, skip, types);
  collectRegex(code, KEYWORD_RE, skip, keywords);
  collectRegex(code, EFFECT_RE, skip, effects);
  return { types, keywords, effects };
}

function collectTypeNames(
  code: string,
  skip: { start: number; end: number }[],
  out: SpanOnLine[],
): void {
  TYPE_NAME_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = TYPE_NAME_RE.exec(code))) {
    if (BUILTIN_TYPE_SET.has(match[0])) continue;
    pushSpan(match, skip, out);
  }
}

function collectRegex(
  code: string,
  re: RegExp,
  skip: { start: number; end: number }[],
  out: SpanOnLine[],
): void {
  re.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = re.exec(code))) {
    pushSpan(match, skip, out);
  }
}

function pushSpan(
  match: RegExpExecArray,
  skip: { start: number; end: number }[],
  out: SpanOnLine[],
): void {
  const start = match.index;
  const length = match[0].length;
  if (length === 0) return;
  if (inside(start, start + length, skip)) return;
  out.push({ start, length });
}

function inside(start: number, end: number, skip: { start: number; end: number }[]): boolean {
  return skip.some((range) => start >= range.start && end <= range.end);
}

function cutComment(line: string): string {
  let i = 0;
  while (i < line.length) {
    if (line[i] === "#") return line.slice(0, i);
    if (line[i] === '"') {
      i = skipString(line, i);
      continue;
    }
    i += 1;
  }
  return line;
}

function opaqueRanges(line: string): { start: number; end: number }[] {
  const ranges: { start: number; end: number }[] = [];
  let i = 0;
  while (i < line.length) {
    if (line[i] === '"') {
      const start = i;
      i = skipString(line, i);
      ranges.push({ start, end: i });
      continue;
    }
    i += 1;
  }
  return ranges;
}

function skipString(line: string, quoteAt: number): number {
  let i = quoteAt + 1;
  while (i < line.length) {
    if (line[i] === "\\") {
      i += 2;
      continue;
    }
    if (line[i] === '"') return i + 1;
    if (line[i] === "{" && line[i + 1] === "{") {
      i += 2;
      continue;
    }
    if (line[i] === "}" && line[i + 1] === "}") {
      i += 2;
      continue;
    }
    i += 1;
  }
  return line.length;
}
