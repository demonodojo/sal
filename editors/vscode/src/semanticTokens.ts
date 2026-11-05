import * as vscode from "vscode";

import { scanSalLine } from "./syntaxScan";

const legend = new vscode.SemanticTokensLegend(["keyword", "type", "effect", "variable"]);

const tokenType = {
  keyword: legend.tokenTypes.indexOf("keyword"),
  type: legend.tokenTypes.indexOf("type"),
  effect: legend.tokenTypes.indexOf("effect"),
  variable: legend.tokenTypes.indexOf("variable"),
};

export function registerSemanticTokens(context: vscode.ExtensionContext): void {
  context.subscriptions.push(
    vscode.languages.registerDocumentSemanticTokensProvider(
      { language: "sal" },
      {
        provideDocumentSemanticTokens(document) {
          const builder = new vscode.SemanticTokensBuilder(legend);
          for (let line = 0; line < document.lineCount; line++) {
            const scanned = scanSalLine(document.lineAt(line).text);
            for (const span of scanned.types) {
              builder.push(line, span.start, span.length, tokenType.type, 0);
            }
            for (const span of scanned.keywords) {
              builder.push(line, span.start, span.length, tokenType.keyword, 0);
            }
            for (const span of scanned.effects) {
              builder.push(line, span.start, span.length, tokenType.effect, 0);
            }
          }
          return builder.build();
        },
      },
      legend,
    ),
  );
}
