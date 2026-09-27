import { readFileSync, statSync } from "node:fs";
import path from "node:path";
import os from "node:os";

import * as vscode from "vscode";

import { definitionsOf, dependencySearchBases, identifierAt } from "./definitions";
import { materializeSource } from "./materialize";
import { spawnProcess } from "./process";
import {
  checkFile,
  checkStyle,
  fixStyle,
  formatFile,
  MISSING_COMPILER_MESSAGE,
  MISSING_STANDARD_MESSAGE,
  styleFixEdits,
  type MappedDiagnostic,
} from "./protocol";

const diagnostics = vscode.languages.createDiagnosticCollection("sal");
const styleFixes = new Map<string, ReturnType<typeof styleFixEdits>>();
const pending = new Map<string, ReturnType<typeof setTimeout>>();
const generation = new Map<string, number>();
let warnedMissingCompiler = false;
let warnedMissingStandard = false;

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(diagnostics);

  context.subscriptions.push(
    vscode.languages.registerDocumentFormattingEditProvider("sal", {
      provideDocumentFormattingEdits: (document) => formatDocument(document),
    }),
  );

  context.subscriptions.push(
    vscode.languages.registerCodeActionsProvider(
      "sal",
      { provideCodeActions: (document, _range, context) => standardQuickFixes(document, context) },
      { providedCodeActionKinds: [vscode.CodeActionKind.QuickFix] },
    ),
  );

  context.subscriptions.push(
    vscode.languages.registerDefinitionProvider("sal", {
      provideDefinition: (document, position) => definitionAt(document, position),
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("sal.check", () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "sal") return;
      return refresh(editor.document);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("sal.format", () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "sal") return;
      return vscode.commands.executeCommand("editor.action.formatDocument");
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("sal.standardFix", () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "sal") return;
      return applyStandardFix(editor.document);
    }),
  );

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => {
      if (document.languageId === "sal") void refresh(document);
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (document.languageId === "sal") void refresh(document);
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidChangeTextDocument((event) => {
      if (event.document.languageId !== "sal") return;
      const key = event.document.uri.toString();
      const previous = pending.get(key);
      if (previous) clearTimeout(previous);
      pending.set(
        key,
        setTimeout(() => {
          pending.delete(key);
          void refresh(event.document);
        }, 400),
      );
    }),
  );
  context.subscriptions.push(
    vscode.workspace.onDidCloseTextDocument((document) => {
      diagnostics.delete(document.uri);
      styleFixes.delete(document.uri.toString());
      const key = document.uri.toString();
      const previous = pending.get(key);
      if (previous) clearTimeout(previous);
      pending.delete(key);
    }),
  );

  for (const document of vscode.workspace.textDocuments) {
    if (document.languageId === "sal") void refresh(document);
  }
}

export function deactivate(): void {
  for (const timer of pending.values()) clearTimeout(timer);
  pending.clear();
}

async function formatDocument(
  document: vscode.TextDocument,
): Promise<vscode.TextEdit[]> {
  const held = await holdSource(document);
  try {
    const outcome = await formatFile({
      command: compilerPath(),
      filePath: held.path,
      cwd: cwdFor(document),
      run: spawnProcess,
    });
    if (outcome.missingCompiler) {
      warnMissingCompiler();
      return [];
    }
    if (outcome.text == null) return [];
    const full = new vscode.Range(
      document.positionAt(0),
      document.positionAt(document.getText().length),
    );
    return [vscode.TextEdit.replace(full, outcome.text)];
  } finally {
    await held.cleanup();
  }
}

async function refresh(document: vscode.TextDocument): Promise<void> {
  const key = document.uri.toString();
  const gen = (generation.get(key) ?? 0) + 1;
  generation.set(key, gen);
  const held = await holdSource(document);
  try {
    const shared = {
      filePath: held.path,
      cwd: cwdFor(document),
      text: document.getText(),
      run: spawnProcess,
    };
    const [compiler, style] = await Promise.all([
      checkFile({ command: compilerPath(), ...shared }),
      checkStyle({ command: standardPath(), ...shared }),
    ]);
    if (generation.get(key) !== gen) return;
    if (compiler.missingCompiler) warnMissingCompiler();
    else warnedMissingCompiler = false;
    if (style.missingCompiler) warnMissingStandard();
    else warnedMissingStandard = false;
    diagnostics.set(document.uri, [
      ...compiler.diagnostics.map((diag) => toVscodeDiagnostic(document, diag)),
      ...style.diagnostics.map((diag) => toVscodeDiagnostic(document, diag)),
    ]);
    styleFixes.set(key, styleFixEdits(style.diagnostics));
  } finally {
    await held.cleanup();
  }
}

function toVscodeDiagnostic(
  document: vscode.TextDocument,
  diag: MappedDiagnostic,
): vscode.Diagnostic {
  const range = new vscode.Range(
    diag.range.startLine,
    diag.range.startCharacter,
    diag.range.endLine,
    diag.range.endCharacter,
  );
  const severity =
    diag.severity === "warning"
      ? vscode.DiagnosticSeverity.Warning
      : vscode.DiagnosticSeverity.Error;
  const item = new vscode.Diagnostic(range, diag.message, severity);
  item.code = diag.code;
  item.source = diag.severity === "warning" ? "standard" : "sal";
  if (diag.hint) {
    item.relatedInformation = [
      new vscode.DiagnosticRelatedInformation(
        new vscode.Location(document.uri, range),
        diag.hint,
      ),
    ];
  }
  return item;
}

function standardQuickFixes(
  document: vscode.TextDocument,
  context: vscode.CodeActionContext,
): vscode.CodeAction[] {
  const edits = styleFixes.get(document.uri.toString()) ?? [];
  const actions: vscode.CodeAction[] = [];
  for (const diag of context.diagnostics) {
    if (diag.source !== "standard") continue;
    const hit = edits.find(
      (edit) =>
        edit.title === diag.message &&
        edit.range.startLine === diag.range.start.line &&
        edit.range.startCharacter === diag.range.start.character &&
        edit.range.endLine === diag.range.end.line &&
        edit.range.endCharacter === diag.range.end.character,
    );
    if (!hit) continue;
    const action = new vscode.CodeAction(hit.title, vscode.CodeActionKind.QuickFix);
    action.diagnostics = [diag];
    action.isPreferred = true;
    const edit = new vscode.WorkspaceEdit();
    edit.replace(document.uri, diag.range, hit.replacement);
    action.edit = edit;
    actions.push(action);
  }
  return actions;
}

async function applyStandardFix(document: vscode.TextDocument): Promise<void> {
  const savedPath = document.uri.scheme === "file" ? document.uri.fsPath : null;
  const directory = savedPath ? path.dirname(savedPath) : cwdFor(document);
  const held = await materializeSource({
    directory,
    savedPath,
    dirty: true,
    text: document.getText(),
  });
  try {
    const outcome = await fixStyle({
      command: standardPath(),
      filePath: held.path,
      cwd: cwdFor(document),
      run: spawnProcess,
      readText: async (filePath) => readFileSync(filePath, "utf8"),
    });
    if (outcome.missingStandard) {
      warnMissingStandard();
      return;
    }
    if (outcome.text == null || outcome.text === document.getText()) return;
    const edit = new vscode.WorkspaceEdit();
    const full = new vscode.Range(
      document.positionAt(0),
      document.positionAt(document.getText().length),
    );
    edit.replace(document.uri, full, outcome.text);
    await vscode.workspace.applyEdit(edit);
  } finally {
    await held.cleanup();
  }
}

async function holdSource(document: vscode.TextDocument): Promise<{
  path: string;
  cleanup: () => Promise<void>;
}> {
  const savedPath = document.uri.scheme === "file" ? document.uri.fsPath : null;
  const directory = savedPath ? path.dirname(savedPath) : cwdFor(document);
  return materializeSource({
    directory,
    savedPath,
    dirty: document.isDirty || savedPath == null,
    text: document.getText(),
  });
}

function cwdFor(document: vscode.TextDocument): string {
  const folder = vscode.workspace.getWorkspaceFolder(document.uri);
  if (folder) return folder.uri.fsPath;
  if (document.uri.scheme === "file") return path.dirname(document.uri.fsPath);
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? os.tmpdir();
}

function definitionAt(
  document: vscode.TextDocument,
  position: vscode.Position,
): vscode.Location[] | null {
  if (document.uri.scheme !== "file") return null;
  const text = document.getText();
  const ident = identifierAt(text, position.line, position.character);
  if (!ident) return null;
  const projectRoot = cwdFor(document);
  let manifest: string | null = null;
  try {
    manifest = readFileSync(path.join(projectRoot, "Sal.toml"), "utf8");
  } catch {
    manifest = null;
  }
  const defs = definitionsOf({
    name: ident.name,
    sourceFile: document.uri.fsPath,
    sourceText: text,
    projectRoot,
    dependencyBases: dependencySearchBases(projectRoot, manifest),
    files: {
      isFile: (filePath) => {
        try {
          return statSync(filePath).isFile();
        } catch {
          return false;
        }
      },
      read: (filePath) => readSalText(filePath),
    },
  });
  if (defs.length === 0) return null;
  const here = path.resolve(document.uri.fsPath);
  return defs.map((def) => {
    const uri =
      path.resolve(def.filePath) === here ? document.uri : vscode.Uri.file(def.filePath);
    const start = new vscode.Position(def.location.line, def.location.character);
    const end = new vscode.Position(
      def.location.line,
      def.location.character + def.location.length,
    );
    return new vscode.Location(uri, new vscode.Range(start, end));
  });
}

function readSalText(filePath: string): string | null {
  const target = path.resolve(filePath);
  const open = vscode.workspace.textDocuments.find(
    (document) =>
      document.uri.scheme === "file" && path.resolve(document.uri.fsPath) === target,
  );
  if (open) return open.getText();
  try {
    return readFileSync(target, "utf8");
  } catch {
    return null;
  }
}

function compilerPath(): string {
  const configured = vscode.workspace.getConfiguration("sal").get<string>("compilerPath");
  if (configured && configured.trim() !== "") return configured;
  return "sal";
}

function standardPath(): string {
  const configured = vscode.workspace.getConfiguration("sal").get<string>("standardPath");
  if (configured && configured.trim() !== "") return configured;
  return "standard";
}

function warnMissingCompiler(): void {
  if (warnedMissingCompiler) return;
  warnedMissingCompiler = true;
  void vscode.window.showWarningMessage(MISSING_COMPILER_MESSAGE);
}

function warnMissingStandard(): void {
  if (warnedMissingStandard) return;
  warnedMissingStandard = true;
  void vscode.window.showWarningMessage(MISSING_STANDARD_MESSAGE);
}
