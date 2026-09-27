export interface SalSpan {
  start: number;
  end: number;
  line: number;
  col: number;
}

export interface SalDiagnostic {
  code: string;
  message: string;
  span: SalSpan;
  hint: string | null;
}

export interface EditorRange {
  startLine: number;
  startCharacter: number;
  endLine: number;
  endCharacter: number;
}

export interface MappedDiagnostic {
  code: string;
  message: string;
  hint: string | null;
  severity: "error" | "warning";
  /** Replacement for a standard quick fix. Null when the warning has no edit. */
  fix: string | null;
  range: EditorRange;
}

export interface StyleFixEdit {
  title: string;
  range: EditorRange;
  replacement: string;
}

export interface ProcessResult {
  stdout: string;
  stderr: string;
  exitCode: number | null;
  notFound: boolean;
}

export type ProcessRunner = (
  command: string,
  args: string[],
  cwd: string,
) => Promise<ProcessResult>;

export interface CheckOutcome {
  diagnostics: MappedDiagnostic[];
  missingCompiler: boolean;
}

export interface FormatOutcome {
  text: string | null;
  missingCompiler: boolean;
}

export const MISSING_COMPILER_MESSAGE =
  "No se encuentra el binario sal. Configura sal.compilerPath.";

export const MISSING_STANDARD_MESSAGE =
  "No se encuentra el binario standard. Configura sal.standardPath.";

export function checkArgs(filePath: string): string[] {
  return ["check", filePath, "--error-format", "json"];
}

export function fmtArgs(filePath: string): string[] {
  return ["fmt", filePath];
}

export function standardCheckArgs(filePath: string): string[] {
  return ["check", filePath, "--error-format", "json"];
}

export function standardFixArgs(filePath: string): string[] {
  return ["fix", filePath];
}

export function parseDiagnosticStderr(stderr: string): SalDiagnostic[] {
  const out: SalDiagnostic[] = [];
  for (const line of stderr.split("\n")) {
    const trimmed = line.trim();
    if (trimmed === "") continue;
    let value: unknown;
    try {
      value = JSON.parse(trimmed);
    } catch {
      continue;
    }
    const diag = asDiagnostic(value);
    if (diag) out.push(diag);
  }
  return out;
}

export function spanToRange(text: string, span: SalSpan): EditorRange {
  const startByte = Math.max(0, span.start);
  const endByte = Math.max(startByte, span.end);
  const start = byteOffsetToPosition(text, startByte, "start");
  const end = byteOffsetToPosition(text, endByte, "end");
  return {
    startLine: start.line,
    startCharacter: start.character,
    endLine: end.line,
    endCharacter: end.character,
  };
}

export function byteOffsetToPosition(
  text: string,
  byteOffset: number,
  edge: "start" | "end",
): { line: number; character: number } {
  const target = byteOffset < 0 ? 0 : byteOffset;
  let line = 0;
  let character = 0;
  let byte = 0;
  for (const ch of text) {
    const size = utf8Len(ch);
    if (byte >= target) return { line, character };
    if (byte + size > target) {
      if (edge === "end") {
        if (ch === "\n") {
          line += 1;
          character = 0;
        } else {
          character += utf16Units(ch);
        }
      }
      return { line, character };
    }
    if (ch === "\n") {
      line += 1;
      character = 0;
    } else {
      character += utf16Units(ch);
    }
    byte += size;
  }
  return { line, character };
}

export async function checkFile(input: {
  command: string;
  filePath: string;
  cwd: string;
  text: string;
  run: ProcessRunner;
}): Promise<CheckOutcome> {
  const result = await input.run(input.command, checkArgs(input.filePath), input.cwd);
  if (result.notFound) {
    return { diagnostics: [], missingCompiler: true };
  }
  if (result.exitCode === 0) {
    return { diagnostics: [], missingCompiler: false };
  }
  const diagnostics = parseDiagnosticStderr(result.stderr).map((diag) => ({
    code: diag.code,
    message: diag.message,
    hint: diag.hint,
    severity: "error" as const,
    fix: null,
    range: spanToRange(input.text, diag.span),
  }));
  return { diagnostics, missingCompiler: false };
}

export async function formatFile(input: {
  command: string;
  filePath: string;
  cwd: string;
  run: ProcessRunner;
}): Promise<FormatOutcome> {
  const result = await input.run(input.command, fmtArgs(input.filePath), input.cwd);
  if (result.notFound) {
    return { text: null, missingCompiler: true };
  }
  if (result.exitCode !== 0) {
    return { text: null, missingCompiler: false };
  }
  return { text: result.stdout, missingCompiler: false };
}

/** `path:line:col: Style/Code: message` from a `standard` that has no JSON format. */
export function parseStandardStderr(
  stderr: string,
): { line: number; col: number; code: string; message: string }[] {
  const out: { line: number; col: number; code: string; message: string }[] = [];
  for (const raw of stderr.split("\n")) {
    const line = raw.trim();
    if (line === "") continue;
    const match = /^(.*):(\d+):(\d+): ([^:]+): (.*)$/.exec(line);
    if (!match) continue;
    const lineNo = Number(match[2]);
    const colNo = Number(match[3]);
    if (!Number.isFinite(lineNo) || !Number.isFinite(colNo)) continue;
    out.push({
      line: lineNo,
      col: colNo,
      code: match[4],
      message: match[5],
    });
  }
  return out;
}

/** One JSON object per line from `standard check --error-format json`. */
export function parseStandardJson(stderr: string): {
  code: string;
  message: string;
  span: SalSpan;
  fix: string | null;
}[] {
  const out: {
    code: string;
    message: string;
    span: SalSpan;
    fix: string | null;
  }[] = [];
  for (const raw of stderr.split("\n")) {
    const line = raw.trim();
    if (line === "") continue;
    let value: unknown;
    try {
      value = JSON.parse(line);
    } catch {
      continue;
    }
    const diag = asStandardJson(value);
    if (diag) out.push(diag);
  }
  return out;
}

export function styleFixEdits(diagnostics: MappedDiagnostic[]): StyleFixEdit[] {
  const out: StyleFixEdit[] = [];
  for (const diag of diagnostics) {
    if (diag.severity !== "warning" || diag.fix == null) continue;
    out.push({
      title: diag.message,
      range: diag.range,
      replacement: diag.fix,
    });
  }
  return out;
}

export function lineColToRange(text: string, line1: number, col1: number): EditorRange {
  const lines = text.split("\n");
  const line = Math.min(Math.max(0, line1 - 1), Math.max(0, lines.length - 1));
  const row = lines[line] ?? "";
  const startCharacter = Math.min(Math.max(0, col1 - 1), row.length);
  return {
    startLine: line,
    startCharacter,
    endLine: line,
    endCharacter: row.length,
  };
}

export async function checkStyle(input: {
  command: string;
  filePath: string;
  cwd: string;
  text: string;
  run: ProcessRunner;
}): Promise<CheckOutcome> {
  const result = await input.run(input.command, standardCheckArgs(input.filePath), input.cwd);
  if (result.notFound) {
    return { diagnostics: [], missingCompiler: true };
  }
  const parsed = parseStandardJson(result.stderr);
  const diagnostics =
    parsed.length > 0
      ? parsed.map((diag) => ({
          code: diag.code,
          message: diag.message,
          hint: null,
          severity: "warning" as const,
          fix: diag.fix,
          range: spanToRange(input.text, diag.span),
        }))
      : parseStandardStderr(result.stderr).map((diag) => ({
          code: diag.code,
          message: diag.message,
          hint: null,
          severity: "warning" as const,
          fix: null,
          range: lineColToRange(input.text, diag.line, diag.col),
        }));
  if (diagnostics.length === 0 && result.exitCode !== 0 && result.exitCode !== null) {
    const raw = result.stderr.trim();
    diagnostics.push({
      code: "standard",
      message: raw.split("\n")[0] || "standard failed",
      hint: null,
      severity: "warning" as const,
      fix: null,
      range: lineColToRange(input.text, 1, 1),
    });
  }
  return { diagnostics, missingCompiler: false };
}

export async function fixStyle(input: {
  command: string;
  filePath: string;
  cwd: string;
  run: ProcessRunner;
  readText: (filePath: string) => Promise<string>;
}): Promise<{ text: string | null; missingStandard: boolean }> {
  const result = await input.run(input.command, standardFixArgs(input.filePath), input.cwd);
  if (result.notFound) {
    return { text: null, missingStandard: true };
  }
  if (result.exitCode !== 0 && result.exitCode !== 1) {
    return { text: null, missingStandard: false };
  }
  return { text: await input.readText(input.filePath), missingStandard: false };
}

function asDiagnostic(value: unknown): SalDiagnostic | null {
  if (!value || typeof value !== "object") return null;
  const rec = value as Record<string, unknown>;
  if (typeof rec.code !== "string" || typeof rec.message !== "string") return null;
  if (!rec.span || typeof rec.span !== "object") return null;
  const span = rec.span as Record<string, unknown>;
  if (
    typeof span.start !== "number" ||
    typeof span.end !== "number" ||
    typeof span.line !== "number" ||
    typeof span.col !== "number"
  ) {
    return null;
  }
  const hint = rec.hint == null ? null : typeof rec.hint === "string" ? rec.hint : null;
  if (rec.hint != null && typeof rec.hint !== "string") return null;
  return {
    code: rec.code,
    message: rec.message,
    span: {
      start: span.start,
      end: span.end,
      line: span.line,
      col: span.col,
    },
    hint,
  };
}

function asStandardJson(value: unknown): {
  code: string;
  message: string;
  span: SalSpan;
  fix: string | null;
} | null {
  const diag = asDiagnostic(value);
  if (!diag) return null;
  const rec = value as Record<string, unknown>;
  if (rec.fix != null && typeof rec.fix !== "string") return null;
  return {
    code: diag.code,
    message: diag.message,
    span: diag.span,
    fix: rec.fix == null ? null : rec.fix,
  };
}

function utf8Len(ch: string): number {
  const cp = ch.codePointAt(0) ?? 0;
  if (cp <= 0x7f) return 1;
  if (cp <= 0x7ff) return 2;
  if (cp <= 0xffff) return 3;
  return 4;
}

function utf16Units(ch: string): number {
  const cp = ch.codePointAt(0) ?? 0;
  return cp > 0xffff ? 2 : 1;
}
