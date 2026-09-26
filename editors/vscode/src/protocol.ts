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
  range: EditorRange;
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

export function checkArgs(filePath: string): string[] {
  return ["check", filePath, "--error-format", "json"];
}

export function fmtArgs(filePath: string): string[] {
  return ["fmt", filePath];
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
