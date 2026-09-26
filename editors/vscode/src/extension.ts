import path from "node:path";
import os from "node:os";

import * as vscode from "vscode";

import { materializeSource } from "./materialize";
import { spawnProcess } from "./process";
import {
  checkFile,
  formatFile,
  MISSING_COMPILER_MESSAGE,
} from "./protocol";

const diagnostics = vscode.languages.createDiagnosticCollection("sal");
const pending = new Map<string, ReturnType<typeof setTimeout>>();
const generation = new Map<string, number>();
let warnedMissing = false;

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(diagnostics);

  context.subscriptions.push(
    vscode.languages.registerDocumentFormattingEditProvider("sal", {
      provideDocumentFormattingEdits: (document) => formatDocument(document),
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
      warnMissing();
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
    const outcome = await checkFile({
      command: compilerPath(),
      filePath: held.path,
      cwd: cwdFor(document),
      text: document.getText(),
      run: spawnProcess,
    });
    if (generation.get(key) !== gen) return;
    if (outcome.missingCompiler) {
      warnMissing();
      diagnostics.set(document.uri, []);
      return;
    }
    warnedMissing = false;
    diagnostics.set(
      document.uri,
      outcome.diagnostics.map((diag) => toVscodeDiagnostic(document, diag)),
    );
  } finally {
    await held.cleanup();
  }
}

function toVscodeDiagnostic(
  document: vscode.TextDocument,
  diag: {
    code: string;
    message: string;
    hint: string | null;
    range: {
      startLine: number;
      startCharacter: number;
      endLine: number;
      endCharacter: number;
    };
  },
): vscode.Diagnostic {
  const range = new vscode.Range(
    diag.range.startLine,
    diag.range.startCharacter,
    diag.range.endLine,
    diag.range.endCharacter,
  );
  const item = new vscode.Diagnostic(range, diag.message, vscode.DiagnosticSeverity.Error);
  item.code = diag.code;
  item.source = "sal";
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

function compilerPath(): string {
  const configured = vscode.workspace.getConfiguration("sal").get<string>("compilerPath");
  if (configured && configured.trim() !== "") return configured;
  return "sal";
}

function warnMissing(): void {
  if (warnedMissing) return;
  warnedMissing = true;
  void vscode.window.showWarningMessage(MISSING_COMPILER_MESSAGE);
}
