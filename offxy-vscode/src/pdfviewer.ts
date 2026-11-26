// The read-only PDF viewer (#2259): `*.pdf` opens in a webview that draws its
// pages with `pdfpages.wasm`, a WebAssembly build of the desktop suite's PDF
// renderer (PdfCraft's pdfcraft-render, suite/pdfpages). Nothing is written
// back, so the document is a plain `CustomDocument`: no edits, save or backup.
// The engine runs in a worker inside the webview (media/pdfworker.js), never in
// the extension host.

import * as vscode from 'vscode';

export const PDF_VIEW_TYPE = 'offxy.pdfViewer';

export class PdfViewerProvider implements vscode.CustomReadonlyEditorProvider {
  constructor(private readonly context: vscode.ExtensionContext) {}

  openCustomDocument(uri: vscode.Uri): vscode.CustomDocument {
    return { uri, dispose: () => {} };
  }

  resolveCustomEditor(document: vscode.CustomDocument, panel: vscode.WebviewPanel): void {
    const media = vscode.Uri.joinPath(this.context.extensionUri, 'media');
    panel.webview.options = { enableScripts: true, localResourceRoots: [media] };
    panel.webview.html = this.html(panel.webview);
    panel.webview.onDidReceiveMessage(async (msg) => {
      if (msg?.type !== 'ready') return;
      try {
        const bytes = await vscode.workspace.fs.readFile(document.uri);
        void panel.webview.postMessage({ type: 'open', bytes });
      } catch (err) {
        void vscode.window.showErrorMessage(`Cannot read ${document.uri.fsPath}: ${err}`);
      }
    });
  }

  private html(webview: vscode.Webview): string {
    const media = (name: string) =>
      webview.asWebviewUri(vscode.Uri.joinPath(this.context.extensionUri, 'media', name)).toString();
    const nonce = makeNonce();
    const csp = [
      `default-src 'none'`,
      `style-src ${webview.cspSource}`,
      `script-src 'nonce-${nonce}' 'wasm-unsafe-eval'`,
      // The engine, its JS and the worker's script are fetched from media/;
      // the worker runs from a blob of those scripts.
      `connect-src ${webview.cspSource}`,
      `worker-src blob:`,
    ].join('; ');
    const cfg = JSON.stringify({
      wasmUri: media('pdfpages.wasm'),
      engineUri: media('pdfengine.js'),
      workerUri: media('pdfworker.js'),
    });
    return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <link href="${media('pdf.css')}" rel="stylesheet" />
  <title>PDF</title>
</head>
<body>
  <div id="toolbar">
    <button id="zoom-out" title="Zoom out (Ctrl+-)" aria-label="Zoom out">−</button>
    <span id="zoom" aria-live="polite">100%</span>
    <button id="zoom-in" title="Zoom in (Ctrl+=)" aria-label="Zoom in">+</button>
    <button id="fit-width" title="Fit width (Ctrl+0)">Fit width</button>
    <label>Page <input id="page" type="number" min="1" value="1" aria-label="Page number" /></label>
    <span id="count"></span>
    <span id="status" role="status"></span>
  </div>
  <div id="pages" tabindex="0" aria-label="Pages"></div>
  <script nonce="${nonce}">window.__OFFXY_PDF__ = ${cfg};</script>
  <script nonce="${nonce}" src="${media('pdfhost.js')}"></script>
  <script nonce="${nonce}" src="${media('pdf.js')}"></script>
</body>
</html>`;
  }
}

function makeNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let s = '';
  for (let i = 0; i < 32; i++) s += chars.charAt(Math.floor(Math.random() * chars.length));
  return s;
}
