import assert from "node:assert/strict";
import { mkdtemp, readFile, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const extensionRoot = path.join(__dirname, "..", "..");

import {
  definitionsOf,
  dependencySearchBases,
  functionDefinitionsIn,
  identifierAt,
  LEXER_WORDS,
  resolveImportPath,
  type TextFiles,
} from "../definitions";
import { materializeSource } from "../materialize";
import {
  checkFile,
  checkStyle,
  fixStyle,
  formatFile,
  styleFixEdits,
  type ProcessResult,
  type ProcessRunner,
  spanToRange,
} from "../protocol";

const text = "fn main()\n";

function parseError(): ProcessResult {
  return {
    stdout: "",
    stderr:
      JSON.stringify({
        code: "E_PARSE",
        message: "unexpected end",
        span: { start: 3, end: 7, line: 1, col: 4 },
        hint: "cierra el paréntesis",
      }) + "\nnot json\n",
    exitCode: 1,
    notFound: false,
  };
}

test("E_PARSE del doble cae en el span de bytes", async () => {
  const calls: string[][] = [];
  const run: ProcessRunner = async (command, args) => {
    calls.push([command, ...args]);
    return parseError();
  };
  const outcome = await checkFile({
    command: "sal",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    text,
    run,
  });
  assert.equal(outcome.missingCompiler, false);
  assert.equal(outcome.diagnostics.length, 1);
  const diag = outcome.diagnostics[0];
  assert.equal(diag.code, "E_PARSE");
  assert.equal(diag.message, "unexpected end");
  assert.equal(diag.hint, "cierra el paréntesis");
  assert.deepEqual(diag.range, {
    startLine: 0,
    startCharacter: 3,
    endLine: 0,
    endCharacter: 7,
  });
  assert.equal(diag.range.startLine, 1 - 1);
  assert.equal(diag.range.startCharacter, 4 - 1);
  assert.deepEqual(calls, [["sal", "check", "/tmp/main.sal", "--error-format", "json"]]);
});

test("un carácter multibyte ocupa sus bytes y sus unidades UTF-16", () => {
  const cafe = spanToRange("café", { start: 3, end: 5, line: 1, col: 4 });
  assert.deepEqual(cafe, {
    startLine: 0,
    startCharacter: 3,
    endLine: 0,
    endCharacter: 4,
  });
  const emoji = spanToRange("fn 😀", { start: 3, end: 7, line: 1, col: 4 });
  assert.deepEqual(emoji, {
    startLine: 0,
    startCharacter: 3,
    endLine: 0,
    endCharacter: 5,
  });
});

test("sal fmt solo sustituye el buffer si sale 0", async () => {
  const canonical = "fn main() -> Int\n    42\n";
  const ok = await formatFile({
    command: "sal",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    run: async () => ({
      stdout: canonical,
      stderr: "",
      exitCode: 0,
      notFound: false,
    }),
  });
  assert.equal(ok.text, canonical);
  assert.equal(ok.missingCompiler, false);

  const failed = await formatFile({
    command: "sal",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    run: async () => ({
      stdout: "borrado",
      stderr: "parse",
      exitCode: 1,
      notFound: false,
    }),
  });
  assert.equal(failed.text, null);
});

test("un binario ausente no inventa un código E_*", async () => {
  const missing: ProcessResult = {
    stdout: "",
    stderr: "",
    exitCode: null,
    notFound: true,
  };
  const outcome = await checkFile({
    command: "sal",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    text,
    run: async () => missing,
  });
  assert.equal(outcome.missingCompiler, true);
  assert.deepEqual(outcome.diagnostics, []);
});

test("standard check ofrece el reemplazo del quick fix", async () => {
  const source = "fn main() -> String ! alloc\n    strdup(\"hi\")\n";
  const calls: string[][] = [];
  const outcome = await checkStyle({
    command: "standard",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    text: source,
    run: async (command, args) => {
      calls.push([command, ...args]);
      return {
        stdout: "",
        stderr:
          JSON.stringify({
            code: "Style/DupString",
            message: "use d\"…\" instead of strdup(\"…\")",
            span: { start: source.indexOf("strdup"), end: source.indexOf("strdup") + "strdup(\"hi\")".length, line: 2, col: 5 },
            fix: "d\"hi\"",
          }) + "\n",
        exitCode: 1,
        notFound: false,
      };
    },
  });
  assert.equal(outcome.missingCompiler, false);
  assert.equal(outcome.diagnostics.length, 1);
  const diag = outcome.diagnostics[0];
  assert.equal(diag.code, "Style/DupString");
  assert.equal(diag.severity, "warning");
  assert.equal(diag.fix, "d\"hi\"");
  const start = source.indexOf("strdup");
  const end = start + "strdup(\"hi\")".length;
  assert.deepEqual(
    diag.range,
    spanToRange(source, { start, end, line: 2, col: 5 }),
  );
  const edits = styleFixEdits(outcome.diagnostics);
  assert.equal(edits.length, 1);
  assert.equal(edits[0].replacement, "d\"hi\"");
  assert.equal(edits[0].title, diag.message);
  assert.deepEqual(calls, [["standard", "check", "/tmp/main.sal", "--error-format", "json"]]);
});

test("standard sin JSON sigue subrayando y no ofrece reemplazo", async () => {
  const source = "fn main() -> Int ! alloc\n    strdup(\"a\")\n";
  const outcome = await checkStyle({
    command: "standard",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    text: source,
    run: async () => ({
      stdout: "",
      stderr: "/tmp/main.sal:2:5: Style/DupString: use d\"…\"\n",
      exitCode: 1,
      notFound: false,
    }),
  });
  assert.equal(outcome.diagnostics.length, 1);
  assert.equal(outcome.diagnostics[0].fix, null);
  assert.equal(styleFixEdits(outcome.diagnostics).length, 0);
  assert.equal(outcome.diagnostics[0].range.startCharacter, 4);
});

test("standard ausente no inventa un aviso", async () => {
  const outcome = await checkStyle({
    command: "standard",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    text,
    run: async () => ({
      stdout: "",
      stderr: "",
      exitCode: null,
      notFound: true,
    }),
  });
  assert.equal(outcome.missingCompiler, true);
  assert.deepEqual(outcome.diagnostics, []);
});

test("standard fix devuelve el texto reescrito si sale 0 o 1", async () => {
  const fixed = "fn main() -> Int ! alloc\n    d\"a\"\n";
  const ok = await fixStyle({
    command: "standard",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    run: async (_command, args) => {
      assert.deepEqual(args, ["fix", "/tmp/main.sal"]);
      return { stdout: "", stderr: "", exitCode: 0, notFound: false };
    },
    readText: async () => fixed,
  });
  assert.equal(ok.missingStandard, false);
  assert.equal(ok.text, fixed);

  const remaining = await fixStyle({
    command: "standard",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    run: async () => ({
      stdout: "",
      stderr: "/tmp/main.sal:1:1: Style/Elsif: ambiguous\n",
      exitCode: 1,
      notFound: false,
    }),
    readText: async () => fixed,
  });
  assert.equal(remaining.text, fixed);

  const lex = await fixStyle({
    command: "standard",
    filePath: "/tmp/main.sal",
    cwd: "/tmp",
    run: async () => ({
      stdout: "",
      stderr: "lex error\n",
      exitCode: 2,
      notFound: false,
    }),
    readText: async () => "no debe leerse",
  });
  assert.equal(lex.text, null);
  assert.equal(lex.missingStandard, false);
});

test("el temporal del buffer sucio se borra", async () => {
  const dir = await mkdtemp(path.join(os.tmpdir(), "sal-editor-"));
  const held = await materializeSource({
    directory: dir,
    savedPath: path.join(dir, "main.sal"),
    dirty: true,
    text: "fn main() -> Int\n    0\n",
  });
  assert.equal(await readFile(held.path, "utf8"), "fn main() -> Int\n    0\n");
  assert.equal(path.dirname(held.path), dir);
  await held.cleanup();
  await assert.rejects(() => stat(held.path));
});

test("los .sal usan el icono de sal", async () => {
  const manifest = JSON.parse(
    await readFile(path.join(extensionRoot, "package.json"), "utf8"),
  ) as {
    icon: string;
    contributes: { languages: { extensions: string[]; icon: { light: string; dark: string } }[] };
  };
  const language = manifest.contributes.languages[0];
  assert.deepEqual(language.extensions, [".sal"]);
  assert.equal(language.icon.light, "./icons/sal.svg");
  assert.equal(language.icon.dark, "./icons/sal.svg");
  assert.equal(manifest.icon, "icons/sal.png");
  const contributes = manifest.contributes as unknown as {
    commands: { command: string }[];
    configuration: { properties: Record<string, { default: string }> };
  };
  assert.ok(contributes.commands.some((cmd) => cmd.command === "sal.standardFix"));
  assert.equal(contributes.configuration.properties["sal.standardPath"].default, "standard");
  const svg = await readFile(path.join(extensionRoot, "icons", "sal.svg"), "utf8");
  assert.match(svg, /<svg/);
  const png = await stat(path.join(extensionRoot, "icons", "sal.png"));
  assert.ok(png.size > 0);
});

test("la gramática colorea las palabras del lexer", async () => {
  const grammar = JSON.parse(
    await readFile(path.join(extensionRoot, "syntaxes", "sal.tmLanguage.json"), "utf8"),
  ) as {
    repository: {
      keyword: { match: string };
      constant: { match: string };
      comment: { match: string };
      dstring: { begin: string };
      type: { match: string };
      tensor: { match: string };
      functionDef: { captures: { "2": { name: string } } };
      "function-def": { captures: { "2": { name: string } } };
      "string-content": { patterns: { name: string }[] };
    };
  };
  const colored = new Set([
    ...alternation(grammar.repository.keyword.match),
    ...alternation(grammar.repository.constant.match),
  ]);
  assert.deepEqual(colored, new Set(LEXER_WORDS));
  assert.match(grammar.repository.comment.match, /#/);
  assert.match(grammar.repository.dstring.begin, /d\\?"/);
  assert.ok(
    grammar.repository["string-content"].patterns.some((pat) => pat.name === "meta.interpolation.sal"),
  );
  assert.equal(
    grammar.repository["function-def"].captures["2"].name,
    "entity.name.function.sal",
  );
  for (const word of ["Tensor", "List", "Dict", "Int", "Float", "Bool", "String", "Unit", "F32", "F16", "BF16", "I8"]) {
    assert.ok(grammar.repository.type.match.includes(word), word);
  }
  assert.match(grammar.repository.tensor.match, /tensor/);
  const language = JSON.parse(
    await readFile(path.join(extensionRoot, "language-configuration.json"), "utf8"),
  ) as {
    comments: { lineComment: string };
    wordPattern: string;
    indentationRules: { increaseIndentPattern: string };
    onEnterRules: { beforeText: string }[];
  };
  assert.equal(language.comments.lineComment, "#");
  assert.match(language.wordPattern, /A-Za-z_/);
  const headers = language.onEnterRules.map((rule) => rule.beforeText).join("\n");
  for (const word of ["fn", "if", "elsif", "while", "match", "on", "struct", "enum"]) {
    assert.match(headers, new RegExp(word));
  }
  assert.match(headers, /else\\s/);
  assert.match(headers, /=>/);
  assert.match(language.indentationRules.increaseIndentPattern, /while/);
  assert.match(language.indentationRules.increaseIndentPattern, /elsif/);
  assert.match(language.indentationRules.increaseIndentPattern, /else\\s/);
});

test("Ctrl+clic sobre una llamada abre el nombre del fn", () => {
  const text = "fn helper() -> Int\n    1\n\nfn main() -> Int\n    helper()\n";
  const callLine = text.split("\n")[4];
  const ident = identifierAt(text, 4, callLine.indexOf("helper"));
  assert.equal(ident?.name, "helper");
  const defs = definitionsOf({
    name: ident?.name ?? "",
    sourceFile: "/proj/main.sal",
    sourceText: text,
    projectRoot: "/proj",
    dependencyBases: [],
    files: memfs({}),
  });
  assert.equal(defs.length, 1);
  assert.equal(defs[0].filePath, path.resolve("/proj/main.sal"));
  assert.deepEqual(defs[0].location, { line: 0, character: 3, length: 6 });
});

test("una función importada se resuelve en el módulo y en el grafo", () => {
  const files = memfs({
    "/proj/main.sal": 'import b\n\nfn main() -> Int\n    helper()\n',
    "/proj/b.sal": 'import "c.sal"\n\nfn other() -> Int\n    0\n',
    "/proj/c.sal": "fn helper() -> Int\n    42\n",
    "/proj/aside.sal": "fn helper() -> Int\n    0\n",
  });
  const defs = definitionsOf({
    name: "helper",
    sourceFile: "/proj/main.sal",
    sourceText: 'import b\n\nfn main() -> Int\n    helper()\n',
    projectRoot: "/proj",
    dependencyBases: [],
    files,
  });
  assert.deepEqual(
    defs.map((def) => def.filePath),
    [path.resolve("/proj/c.sal")],
  );
  assert.equal(defs[0].location.character, 3);
});

test("el fn local gana al importado y una palabra del lexer no es una función", () => {
  const text = 'import "b.sal"\n\nfn helper() -> Int\n    1\n';
  const files = memfs({
    "/proj/b.sal": "fn helper() -> Int\n    2\n",
  });
  const local = definitionsOf({
    name: "helper",
    sourceFile: "/proj/main.sal",
    sourceText: text,
    projectRoot: "/proj",
    dependencyBases: [],
    files,
  });
  assert.deepEqual(
    local.map((def) => def.filePath),
    [path.resolve("/proj/main.sal")],
  );
  assert.deepEqual(
    definitionsOf({
      name: "if",
      sourceFile: "/proj/main.sal",
      sourceText: text,
      projectRoot: "/proj",
      dependencyBases: [],
      files,
    }),
    [],
  );
});

test("un fn comentado, una cadena y un import no son destinos", () => {
  const commented = "# fn helper() -> Int\nfn main() -> Int\n    0\n";
  assert.deepEqual(
    functionDefinitionsIn(commented).map((def) => def.name),
    ["main"],
  );
  const quoted = 'fn main() -> Int\n    print("helper")\n';
  const quotedLine = quoted.split("\n")[1];
  assert.equal(identifierAt(quoted, 1, quotedLine.indexOf("helper")), null);
  const imported = "import helper\n\nfn main() -> Int\n    0\n";
  assert.equal(identifierAt(imported, 0, imported.indexOf("helper")), null);
  const interp = 'fn helper() -> Int\n    1\n\nfn main() -> Int\n    "{helper}"\n';
  const interpLine = interp.split("\n")[4];
  assert.equal(identifierAt(interp, 4, interpLine.indexOf("helper"))?.name, "helper");
});

test("Sal.toml añade la base de un path de dependencia", () => {
  const manifest = '[package]\nname = "app"\n\n[dependencies]\nwidgets = { path = "vendor/widgets" }\n';
  const bases = dependencySearchBases("/proj", manifest);
  assert.deepEqual(bases, [path.resolve("/proj/vendor/widgets")]);
  const files = memfs({
    "/proj/vendor/widgets/util.sal": "fn widget() -> Int\n    1\n",
  });
  const hit = resolveImportPath(
    "widgets/util.sal",
    "/proj/main.sal",
    "/proj",
    bases,
    files.isFile,
  );
  assert.equal(hit, path.resolve("/proj/vendor/widgets/util.sal"));
});

function alternation(match: string): string[] {
  const inner = match.match(/\(\?:([^)]+)\)/);
  assert.ok(inner);
  return inner[1].split("|");
}

function memfs(files: Record<string, string>): TextFiles {
  const map = new Map(Object.entries(files).map(([file, text]) => [path.resolve(file), text]));
  return {
    isFile: (filePath) => map.has(path.resolve(filePath)),
    read: (filePath) => map.get(path.resolve(filePath)) ?? null,
  };
}

test("un buffer guardado no se copia", async () => {
  const saved = "/workspace/main.sal";
  const held = await materializeSource({
    directory: "/workspace",
    savedPath: saved,
    dirty: false,
    text: "fn main() -> Int\n    0\n",
  });
  assert.equal(held.path, saved);
  await held.cleanup();
});
