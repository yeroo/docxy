// Docxy VS Code extension — a binary custom editor for Word `.docx` files.
//
// Architecture: the whole DOCX engine (parse → render → edit → lossless save) is
// a WebAssembly build of the dependency-free `docxcore` Rust crate, and it runs
// *inside the webview*. The extension host is a thin coordinator: it hands the
// file bytes to the webview on open, relays VS Code's undo/redo and save into the
// webview, and writes the bytes the webview serializes back out. Editing stays
// local to the webview (no host round-trip per keystroke), while VS Code still
// owns the dirty indicator, undo stack, save, and hot-exit backups through the
// standard CustomEditor edit events.

import * as vscode from 'vscode';

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(DocxyEditorProvider.register(context));
}

export function deactivate(): void {
  /* nothing to clean up */
}

/** A live `.docx` document. The authoritative content lives in the webview's
 *  wasm session; this object just holds identity plus the on-disk bytes needed
 *  to (re)open, and coordinates request/response with its webview. */
class DocxDocument implements vscode.CustomDocument {
  private readonly pending = new Map<number, (value: Uint8Array) => void>();
  private reqSeq = 0;
  /** Set once the editor panel is resolved; used to message the webview. */
  panel?: vscode.WebviewPanel;

  constructor(
    public readonly uri: vscode.Uri,
    public readonly initialContent: Uint8Array,
  ) {}

  dispose(): void {
    this.pending.clear();
  }

  /** Ask the webview to serialize the current document and resolve with the
   *  resulting `.docx` bytes. */
  requestBytes(): Promise<Uint8Array> {
    const panel = this.panel;
    if (!panel) {
      // No live webview (e.g. hidden without retained context): fall back to the
      // last-known on-disk bytes so save/backup never rejects.
      return Promise.resolve(this.initialContent);
    }
    const requestId = ++this.reqSeq;
    return new Promise<Uint8Array>((resolve) => {
      this.pending.set(requestId, resolve);
      panel.webview.postMessage({ type: 'getBytes', requestId });
    });
  }

  /** Resolve a pending `requestBytes` call with bytes returned by the webview. */
  fulfillBytes(requestId: number, bytes: Uint8Array): void {
    const resolve = this.pending.get(requestId);
    if (resolve) {
      this.pending.delete(requestId);
      resolve(bytes);
    }
  }
}

class DocxyEditorProvider implements vscode.CustomEditorProvider<DocxDocument> {
  private static readonly viewType = 'docxy.docxEditor';

  /** The most recently focused Docxy panel, so command-palette actions target
   *  the editor the user is looking at. */
  private activePanel?: vscode.WebviewPanel;

  static register(context: vscode.ExtensionContext): vscode.Disposable {
    const provider = new DocxyEditorProvider(context);
    const disposables: vscode.Disposable[] = [
      vscode.window.registerCustomEditorProvider(
        DocxyEditorProvider.viewType,
        provider,
        {
          webviewOptions: { retainContextWhenHidden: true },
          supportsMultipleEditorsPerDocument: false,
        },
      ),
    ];
    // Register the command-palette actions once; each posts a bridge command
    // string (tab-delimited) to the active panel's webview.
    const COMMANDS: Array<[string, string]> = [
      ['docxy.toggleBold', 'bold'],
      ['docxy.toggleItalic', 'italic'],
      ['docxy.toggleUnderline', 'underline'],
      ['docxy.toggleStrike', 'strike'],
      ['docxy.heading1', 'heading\t1'],
      ['docxy.heading2', 'heading\t2'],
      ['docxy.heading3', 'heading\t3'],
      ['docxy.normalStyle', 'heading\t0'],
      ['docxy.bulletList', 'list\tbullet'],
      ['docxy.numberedList', 'list\tnumber'],
      ['docxy.alignLeft', 'align\tleft'],
      ['docxy.alignCenter', 'align\tcenter'],
      ['docxy.alignRight', 'align\tright'],
      ['docxy.alignJustify', 'align\tjustify'],
      ['docxy.fontBigger', 'fontsize\t2'],
      ['docxy.fontSmaller', 'fontsize\t-2'],
    ];
    for (const [cmd, op] of COMMANDS) {
      disposables.push(
        vscode.commands.registerCommand(cmd, () => {
          provider.activePanel?.webview.postMessage({ type: 'command', op });
        }),
      );
    }
    return vscode.Disposable.from(...disposables);
  }

  constructor(private readonly context: vscode.ExtensionContext) {}

  // --- edit / dirty / undo-redo plumbing ------------------------------------

  private readonly _onDidChange =
    new vscode.EventEmitter<vscode.CustomDocumentEditEvent<DocxDocument>>();
  readonly onDidChangeCustomDocument = this._onDidChange.event;

  // --- document lifecycle ---------------------------------------------------

  async openCustomDocument(uri: vscode.Uri): Promise<DocxDocument> {
    const content =
      uri.scheme === 'untitled'
        ? new Uint8Array()
        : await vscode.workspace.fs.readFile(uri);
    return new DocxDocument(uri, content);
  }

  async resolveCustomEditor(
    document: DocxDocument,
    panel: vscode.WebviewPanel,
  ): Promise<void> {
    document.panel = panel;
    panel.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.context.extensionUri, 'media')],
    };
    panel.webview.html = this.html(panel.webview);

    const sub = panel.webview.onDidReceiveMessage((msg) =>
      this.onMessage(document, panel, msg),
    );
    if (panel.active) {
      this.activePanel = panel;
    }
    const viewSub = panel.onDidChangeViewState((e) => {
      if (e.webviewPanel.active) {
        this.activePanel = e.webviewPanel;
      }
    });
    panel.onDidDispose(() => {
      sub.dispose();
      viewSub.dispose();
      if (document.panel === panel) {
        document.panel = undefined;
      }
      if (this.activePanel === panel) {
        this.activePanel = undefined;
      }
    });
  }

  private onMessage(
    document: DocxDocument,
    panel: vscode.WebviewPanel,
    msg: any,
  ): void {
    switch (msg?.type) {
      case 'ready':
        // Hand the webview the file bytes (base64) to open in wasm.
        panel.webview.postMessage({
          type: 'open',
          data: Buffer.from(document.initialContent).toString('base64'),
        });
        break;

      case 'edit':
        // A mutating edit happened in the webview. Register it with VS Code so
        // the dirty indicator lights and Ctrl-Z/Ctrl-Y route back to the wasm
        // editor's own undo stack (kept in lockstep — one edit per event).
        this._onDidChange.fire({
          document,
          label: msg.label || 'Edit',
          undo: () => {
            void panel.webview.postMessage({ type: 'do', op: 'undo' });
          },
          redo: () => {
            void panel.webview.postMessage({ type: 'do', op: 'redo' });
          },
        });
        break;

      case 'bytes':
        document.fulfillBytes(msg.requestId, new Uint8Array(Buffer.from(msg.data, 'base64')));
        break;

      case 'clipboard':
        void vscode.env.clipboard.writeText(msg.text ?? '');
        break;

      case 'readClipboard':
        void vscode.env.clipboard.readText().then((text) =>
          panel.webview.postMessage({ type: 'clipboardText', requestId: msg.requestId, text }),
        );
        break;

      case 'openLink':
        if (typeof msg.href === 'string') {
          void vscode.env.openExternal(vscode.Uri.parse(msg.href));
        }
        break;
    }
  }

  // --- save / backup --------------------------------------------------------

  async saveCustomDocument(
    document: DocxDocument,
    _cancellation: vscode.CancellationToken,
  ): Promise<void> {
    await this.saveAs(document, document.uri);
  }

  async saveCustomDocumentAs(
    document: DocxDocument,
    destination: vscode.Uri,
    _cancellation: vscode.CancellationToken,
  ): Promise<void> {
    await this.saveAs(document, destination);
  }

  private async saveAs(document: DocxDocument, target: vscode.Uri): Promise<void> {
    const bytes = await document.requestBytes();
    await vscode.workspace.fs.writeFile(target, bytes);
  }

  async revertCustomDocument(document: DocxDocument): Promise<void> {
    const bytes = await vscode.workspace.fs.readFile(document.uri);
    document.panel?.webview.postMessage({
      type: 'open',
      data: Buffer.from(bytes).toString('base64'),
    });
  }

  async backupCustomDocument(
    document: DocxDocument,
    ctx: vscode.CustomDocumentBackupContext,
    _cancellation: vscode.CancellationToken,
  ): Promise<vscode.CustomDocumentBackup> {
    const bytes = await document.requestBytes();
    await vscode.workspace.fs.writeFile(ctx.destination, bytes);
    return {
      id: ctx.destination.toString(),
      delete: async () => {
        try {
          await vscode.workspace.fs.delete(ctx.destination);
        } catch {
          /* already gone */
        }
      },
    };
  }

  // --- webview HTML ---------------------------------------------------------

  private html(webview: vscode.Webview): string {
    const media = (name: string) =>
      webview.asWebviewUri(vscode.Uri.joinPath(this.context.extensionUri, 'media', name));
    const scriptUri = media('webview.js');
    const styleUri = media('webview.css');
    const wasmUri = media('docxwasm.wasm');
    const nonce = makeNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${webview.cspSource} data:`,
      `style-src ${webview.cspSource}`,
      `font-src ${webview.cspSource}`,
      // wasm needs 'unsafe-eval' to compile in some webview runtimes; scope it to
      // our nonce'd script only.
      `script-src 'nonce-${nonce}' 'wasm-unsafe-eval'`,
      `connect-src ${webview.cspSource}`,
    ].join('; ');

    return /* html */ `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <link href="${styleUri}" rel="stylesheet" />
  <title>Docxy</title>
</head>
<body>
  <div id="doc" tabindex="0" aria-label="Document" spellcheck="false"></div>
  <div id="status" role="status"></div>
  <script nonce="${nonce}">window.__DOCXY__ = { wasmUri: "${wasmUri}" };</script>
  <script nonce="${nonce}" src="${scriptUri}"></script>
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
