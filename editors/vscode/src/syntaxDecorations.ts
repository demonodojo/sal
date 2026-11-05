import * as vscode from "vscode";

import { scanSalLine } from "./syntaxScan";

const TYPE_COLOR = "#89D185";
const KEYWORD_COLOR = "#C586C0";
const EFFECT_COLOR = "#DCDCAA";

export function registerSyntaxDecorations(context: vscode.ExtensionContext): void {
  const typeStyle = vscode.window.createTextEditorDecorationType({
    color: TYPE_COLOR,
    fontWeight: "600",
  });
  const keywordStyle = vscode.window.createTextEditorDecorationType({
    color: KEYWORD_COLOR,
  });
  const effectStyle = vscode.window.createTextEditorDecorationType({
    color: EFFECT_COLOR,
    fontStyle: "italic",
  });
  context.subscriptions.push(typeStyle, keywordStyle, effectStyle);

  const paint = (editor: vscode.TextEditor): void => {
    if (editor.document.languageId !== "sal") {
      clearDecorations(editor, typeStyle, keywordStyle, effectStyle);
      return;
    }
    if (!vscode.workspace.getConfiguration("sal").get<boolean>("syntaxDecorations", true)) {
      clearDecorations(editor, typeStyle, keywordStyle, effectStyle);
      return;
    }
    const typeRanges: vscode.Range[] = [];
    const keywordRanges: vscode.Range[] = [];
    const effectRanges: vscode.Range[] = [];
    const doc = editor.document;
    for (let line = 0; line < doc.lineCount; line++) {
      const scanned = scanSalLine(doc.lineAt(line).text);
      for (const span of scanned.types) {
        typeRanges.push(
          new vscode.Range(line, span.start, line, span.start + span.length),
        );
      }
      for (const span of scanned.keywords) {
        keywordRanges.push(
          new vscode.Range(line, span.start, line, span.start + span.length),
        );
      }
      for (const span of scanned.effects) {
        effectRanges.push(
          new vscode.Range(line, span.start, line, span.start + span.length),
        );
      }
    }
    editor.setDecorations(typeStyle, typeRanges);
    editor.setDecorations(keywordStyle, keywordRanges);
    editor.setDecorations(effectStyle, effectRanges);
  };

  const paintVisible = (): void => {
    for (const editor of vscode.window.visibleTextEditors) {
      paint(editor);
    }
  };

  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor((editor) => {
      if (editor) paint(editor);
    }),
    vscode.workspace.onDidChangeTextDocument((event) => {
      for (const editor of vscode.window.visibleTextEditors) {
        if (editor.document === event.document) paint(editor);
      }
    }),
    vscode.workspace.onDidOpenTextDocument(() => paintVisible()),
    vscode.workspace.onDidCloseTextDocument(() => paintVisible()),
  );

  paintVisible();
}

function clearDecorations(
  editor: vscode.TextEditor,
  typeStyle: vscode.TextEditorDecorationType,
  keywordStyle: vscode.TextEditorDecorationType,
  effectStyle: vscode.TextEditorDecorationType,
): void {
  editor.setDecorations(typeStyle, []);
  editor.setDecorations(keywordStyle, []);
  editor.setDecorations(effectStyle, []);
}
