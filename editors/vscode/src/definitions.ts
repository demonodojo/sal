import path from "node:path";

/** Palabras que el lexer emite como token propio, no como identificador. */
export const LEXER_WORDS = [
  "fn",
  "let",
  "if",
  "elsif",
  "else",
  "match",
  "on",
  "to",
  "kernel",
  "return",
  "true",
  "false",
  "import",
  "try",
  "while",
  "struct",
  "enum",
  "parallel",
  "cpu",
  "gpu",
  "tpu",
] as const;

const LEXER_WORD_SET = new Set<string>(LEXER_WORDS);

export interface SourceSpan {
  line: number;
  character: number;
  length: number;
}

export interface NamedSpan {
  name: string;
  location: SourceSpan;
}

export interface FunctionDefinition extends NamedSpan {
  filePath: string;
}

export interface TextFiles {
  isFile(filePath: string): boolean;
  read(filePath: string): string | null;
}

export function splitLines(text: string): string[] {
  const lines = text.split("\n");
  return lines.map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line));
}

export function functionDefinitionsIn(text: string): NamedSpan[] {
  const out: NamedSpan[] = [];
  const lines = splitLines(text);
  for (let line = 0; line < lines.length; line++) {
    const code = cutComment(lines[line]);
    const match = /^fn[ \t]+([A-Za-z_][A-Za-z0-9_]*)/.exec(code);
    if (!match) continue;
    const name = match[1];
    out.push({
      name,
      location: {
        line,
        character: match[0].length - name.length,
        length: name.length,
      },
    });
  }
  return out;
}

export function importsIn(text: string): string[] {
  const out: string[] = [];
  for (const line of splitLines(text)) {
    const code = cutComment(line).trim();
    const quoted = /^import[ \t]+"([^"]*)"/.exec(code);
    if (quoted) {
      if (quoted[1] !== "") out.push(quoted[1]);
      continue;
    }
    const ident = /^import[ \t]+([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)/.exec(code);
    if (ident) out.push(ident[1]);
  }
  return out;
}

export function identifierAt(text: string, line: number, character: number): NamedSpan | null {
  const lines = splitLines(text);
  if (line < 0 || line >= lines.length) return null;
  const source = lines[line];
  if (/^[ \t]*import\b/.test(cutComment(source))) return null;
  const ranges = navigableRanges(source);
  const ident = /[A-Za-z_][A-Za-z0-9_]*/g;
  let atEnd: NamedSpan | null = null;
  let match: RegExpExecArray | null;
  while ((match = ident.exec(source))) {
    const start = match.index;
    const end = start + match[0].length;
    const inside = ranges.some((range) => start >= range.start && end <= range.end);
    if (!inside) continue;
    const hit: NamedSpan = {
      name: match[0],
      location: { line, character: start, length: match[0].length },
    };
    if (character >= start && character < end) return hit;
    if (character === end) atEnd = hit;
  }
  return atEnd;
}

export function dependencySearchBases(projectRoot: string, manifest: string | null): string[] {
  if (manifest == null) return [];
  const bases: string[] = [];
  let inDeps = false;
  for (const raw of splitLines(manifest)) {
    const line = raw.trim();
    if (line.startsWith("[")) {
      inDeps = line === "[dependencies]";
      continue;
    }
    if (!inDeps) continue;
    const needle = 'path = "';
    const at = line.indexOf(needle);
    if (at < 0) continue;
    const rest = line.slice(at + needle.length);
    const end = rest.indexOf('"');
    if (end <= 0) continue;
    bases.push(path.resolve(projectRoot, rest.slice(0, end)));
  }
  return bases;
}

export function resolveImportPath(
  importPath: string,
  sourceFile: string,
  projectRoot: string,
  dependencyBases: string[],
  isFile: (filePath: string) => boolean,
): string | null {
  const candidates: string[] = [];
  pushCandidates(candidates, path.dirname(sourceFile), importPath);
  pushCandidates(candidates, projectRoot, importPath);
  for (const base of dependencyBases) {
    pushCandidates(candidates, base, importPath);
    const slash = importPath.indexOf("/");
    if (slash > 0 && path.basename(base) === importPath.slice(0, slash)) {
      pushCandidates(candidates, base, importPath.slice(slash + 1));
    }
  }
  for (const candidate of candidates) {
    if (isFile(candidate)) return candidate;
  }
  return null;
}

export function definitionsOf(input: {
  name: string;
  sourceFile: string;
  sourceText: string;
  projectRoot: string;
  dependencyBases: string[];
  files: TextFiles;
}): FunctionDefinition[] {
  if (LEXER_WORD_SET.has(input.name)) return [];
  const sourceFile = path.resolve(input.sourceFile);
  const local = functionDefinitionsIn(input.sourceText).filter((def) => def.name === input.name);
  if (local.length > 0) {
    return local.map((def) => ({ filePath: sourceFile, name: def.name, location: def.location }));
  }

  const projectRoot = path.resolve(input.projectRoot);
  const visited = new Set<string>([sourceFile]);
  const found: FunctionDefinition[] = [];
  const queue: { filePath: string; text: string }[] = [
    { filePath: sourceFile, text: input.sourceText },
  ];
  while (queue.length > 0) {
    const current = queue.shift();
    if (!current) break;
    for (const spec of importsIn(current.text)) {
      const resolved = resolveImportPath(
        spec,
        current.filePath,
        projectRoot,
        input.dependencyBases,
        input.files.isFile,
      );
      if (!resolved) continue;
      const key = path.resolve(resolved);
      if (visited.has(key)) continue;
      visited.add(key);
      const text = input.files.read(key);
      if (text == null) continue;
      for (const def of functionDefinitionsIn(text)) {
        if (def.name === input.name) {
          found.push({ filePath: key, name: def.name, location: def.location });
        }
      }
      queue.push({ filePath: key, text });
    }
  }
  return found;
}

function pushCandidates(out: string[], base: string, importPath: string): void {
  out.push(path.resolve(base, importPath));
  if (!importPath.endsWith(".sal")) {
    out.push(path.resolve(base, `${importPath}.sal`));
  }
}

function cutComment(line: string): string {
  let i = 0;
  while (i < line.length) {
    if (line[i] === "#") return line.slice(0, i);
    if (line[i] === '"') {
      i = endOfString(line, i);
      continue;
    }
    i += 1;
  }
  return line;
}

function navigableRanges(line: string): { start: number; end: number }[] {
  const ranges: { start: number; end: number }[] = [];
  let i = 0;
  let regionStart = 0;
  const close = (at: number) => {
    if (at > regionStart) ranges.push({ start: regionStart, end: at });
  };
  while (i < line.length) {
    if (line[i] === "#") {
      close(i);
      return ranges;
    }
    if (line[i] === '"') {
      close(i);
      i = scanString(line, i, ranges);
      regionStart = i;
      continue;
    }
    i += 1;
  }
  close(line.length);
  return ranges;
}

function scanString(
  line: string,
  quoteAt: number,
  ranges: { start: number; end: number }[],
): number {
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
    if (line[i] === "{") {
      const ident = /^[A-Za-z_][A-Za-z0-9_]*/.exec(line.slice(i + 1));
      if (ident && line[i + 1 + ident[0].length] === "}") {
        ranges.push({ start: i + 1, end: i + 1 + ident[0].length });
        i += 1 + ident[0].length + 1;
        continue;
      }
    }
    i += 1;
  }
  return line.length;
}

function endOfString(line: string, quoteAt: number): number {
  return scanString(line, quoteAt, []);
}
