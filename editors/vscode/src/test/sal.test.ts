import assert from "node:assert/strict";
import { mkdtemp, readFile, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const extensionRoot = path.join(__dirname, "..", "..");

import { materializeSource } from "../materialize";
import {
  checkFile,
  formatFile,
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
  const svg = await readFile(path.join(extensionRoot, "icons", "sal.svg"), "utf8");
  assert.match(svg, /<svg/);
  const png = await stat(path.join(extensionRoot, "icons", "sal.png"));
  assert.ok(png.size > 0);
});

test("la gramática colorea las palabras del lexer", async () => {
  const grammar = JSON.parse(
    await readFile(path.join(extensionRoot, "syntaxes", "sal.tmLanguage.json"), "utf8"),
  ) as { repository: { keyword: { match: string }; constant: { match: string }; comment: { match: string } } };
  const colored = grammar.repository.keyword.match + grammar.repository.constant.match;
  for (const word of [
    "fn", "let", "if", "match", "on", "to", "kernel", "return",
    "import", "try", "struct", "enum", "cpu", "gpu", "tpu", "true", "false",
  ]) {
    assert.ok(colored.includes(word), word);
  }
  assert.match(grammar.repository.comment.match, /#/);
  const language = JSON.parse(
    await readFile(path.join(extensionRoot, "language-configuration.json"), "utf8"),
  ) as { comments: { lineComment: string }; onEnterRules: { beforeText: string }[] };
  assert.equal(language.comments.lineComment, "#");
  const headers = language.onEnterRules.map((rule) => rule.beforeText).join("\n");
  for (const word of ["fn", "if", "match", "on", "struct", "enum"]) {
    assert.match(headers, new RegExp(word));
  }
  assert.match(headers, /=>/);
});

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
