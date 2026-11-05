import * as vscode from "vscode";

export function isSalPath(uri: vscode.Uri): boolean {
  return uri.fsPath.endsWith(".sal");
}

/** Cursor a veces abre `.sal` como Plain Text; sin `sal` la extensión no colorea. */
export async function ensureSalLanguage(document: vscode.TextDocument): Promise<void> {
  if (!isSalPath(document.uri)) return;
  if (document.languageId === "sal") return;
  await vscode.languages.setTextDocumentLanguage(document, "sal");
}

export function registerSalLanguageMode(context: vscode.ExtensionContext): void {
  const fix = (document: vscode.TextDocument) => {
    if (isSalPath(document.uri)) void ensureSalLanguage(document);
  };
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument(fix),
    vscode.workspace.onDidSaveTextDocument(fix),
  );
  for (const document of vscode.workspace.textDocuments) {
    fix(document);
  }
}
