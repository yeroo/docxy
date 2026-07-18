// Offxy VS Code extension — binary custom editors for Office files (currently
// Word `.docx`; more formats register via the `EDITORS` table below).
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
import { docxToMarkdown, markdownToDocx } from './engine';
import { newWorkbook } from './gridengine';
import { CtlHost, CtlServer } from './ctlserver';

export function activate(context: vscode.ExtensionContext): void {
  context.subscriptions.push(register(context));
}

export function deactivate(): void {
  /* nothing to clean up */
}

/** Describes one binary custom-editor registration. Each entry in `EDITORS`
 *  spins up its own `OffxyEditorProvider` instance bound to these assets. */
interface EditorSpec {
  /** Custom-editor view type, e.g. `'offxy.docxEditor'`. */
  viewType: string;
  /** Human-readable name used in messages (e.g. "Word document"). */
  label: string;
  /** `media/` script file name. */
  script: string;
  /** `media/` stylesheet file name. */
  style: string;
  /** `media/` wasm file name. */
  wasm: string;
  /** Modal body text offered when a 0-byte file of this type is opened. May
   *  contain the literal substring `{name}`, which is replaced with the
   *  file's basename before display. */
  emptyPrompt?: string;
  /** Mint a fresh empty document's bytes for the empty-file create flow (both
   *  the open-time modal and the webview's in-tab `createNew` message). */
  mintEmpty?: (context: vscode.ExtensionContext) => Promise<Uint8Array>;
  /** Agent control-surface identity for this editor's tabs: which terminal
   *  app they masquerade as on the ctl wire (`docs/agent-control.md`), plus
   *  the wasm verb sets `CtlServer` needs — which verbs route to
   *  `docx_ctl`/`grid_ctl` at all, and which of those mark the document
   *  modified / drive the VS Code undo stack. */
  ctl: {
    app: 'docxy' | 'xlsxy';
    wasmVerbs: Set<string>;
    mutatingVerbs: Set<string>;
  };
}

const EDITORS: EditorSpec[] = [
  {
    viewType: 'offxy.docxEditor',
    label: 'Word document',
    script: 'webview.js',
    style: 'webview.css',
    wasm: 'docxwasm.wasm',
    emptyPrompt:
      '“{name}” is empty — it isn\'t a Word document yet. Create a new Word document in its place?',
    mintEmpty: (ctx) => markdownToDocx(ctx, ''),
    ctl: {
      app: 'docxy',
      // 'doc.blocks' is deliberately NOT in this set: it's an internal-only
      // verb `fullPathInfo()` calls directly (bypassing this gate), used to
      // compose `doc.path`'s reply. Terminal docxy's control.rs has no
      // `doc.blocks` arm, so exposing it here to external agents would let a
      // VS Code tab answer a verb a terminal instance rejects as "unknown
      // verb" — breaking "indistinguishable from a terminal instance".
      wasmVerbs: new Set([
        'doc.outline',
        'doc.read',
        'doc.find',
        'doc.replace-range',
        'doc.insert',
        'doc.append',
      ]),
      mutatingVerbs: new Set(['doc.replace-range', 'doc.insert', 'doc.append']),
    },
  },
  {
    viewType: 'offxy.gridEditor',
    label: 'Excel workbook',
    script: 'grid.js',
    style: 'grid.css',
    wasm: 'gridwasm.wasm',
    emptyPrompt:
      '“{name}” is empty — it isn\'t an Excel workbook yet. Create a new workbook in its place?',
    mintEmpty: (ctx) => newWorkbook(ctx),
    ctl: {
      app: 'xlsxy',
      // 'wb.info' is deliberately NOT in this set: same reasoning as
      // 'doc.blocks' above — it's an internal-only verb `fullPathInfo()`
      // calls directly to compose `wb.path`'s reply, and terminal xlsxy's
      // control.rs has no `wb.info` arm.
      wasmVerbs: new Set([
        'sheet.list',
        'sheet.read',
        'cell.get',
        'cell.set',
        'range.clear',
        'find',
        'wb.recalc',
      ]),
      mutatingVerbs: new Set(['cell.set', 'range.clear']),
    },
  },
];

/** Every ctl server this extension host has started, across both editor
 *  providers — so extension deactivate can tear all of them down even if a
 *  panel's own dispose somehow didn't run first (VS Code normally disposes
 *  every webview panel before deactivating, so this is a safety net). */
const activeCtlServers = new Set<CtlServer>();

/** Monotonic counter backing each ctl instance's suffix, so two tabs with the
 *  same basename (e.g. `report.docx` opened from two different folders) get
 *  distinct discovery filenames instead of colliding. */
let ctlInstanceSeq = 0;

/** Build a `CtlServer` instance suffix: a filesystem-safe basename plus a
 *  per-extension-session sequence number. */
function nextCtlSuffix(uri: vscode.Uri): string {
  const safe = basename(uri).replace(/[^A-Za-z0-9._-]/g, '_') || 'doc';
  return `${safe}-${++ctlInstanceSeq}`;
}

/** Pull the `verb` string out of a ctl request JSON string (as sent to
 *  `CtlHost.callWasm`), or `undefined` if it doesn't parse / isn't a string —
 *  used to decide whether a webview repaint is needed after the call. */
function ctlVerbOf(requestJson: string): string | undefined {
  try {
    const verb = JSON.parse(requestJson)?.verb;
    return typeof verb === 'string' ? verb : undefined;
  } catch {
    return undefined;
  }
}

/** Parse a raw wasm ctl reply (`docx_ctl`/`grid_ctl`'s flat
 *  `{...fields,"ok":true}` / `{"ok":false,"error":…}` envelope) and return its
 *  fields when it succeeded, or `undefined` on failure or a parse error. Used
 *  by the provider's own internal `doc.blocks`/`wb.info`/`sheet.list` calls
 *  that compose `pathInfo()`/`save()`/`reload()` replies. */
function ctlOkResult(raw: string): Record<string, unknown> | undefined {
  try {
    const parsed = JSON.parse(raw);
    return parsed && parsed.ok === true ? (parsed as Record<string, unknown>) : undefined;
  } catch {
    return undefined;
  }
}

function register(context: vscode.ExtensionContext): vscode.Disposable {
  const disposables: vscode.Disposable[] = [];
  let docxProvider: OffxyEditorProvider | undefined;

  for (const spec of EDITORS) {
    const provider = new OffxyEditorProvider(context, spec);
    disposables.push(
      vscode.window.registerCustomEditorProvider(spec.viewType, provider, {
        // enableFindWidget lets Ctrl+F search the rendered document text (it's
        // real DOM text in the webview) with no extra code.
        webviewOptions: { retainContextWhenHidden: true, enableFindWidget: true },
        supportsMultipleEditorsPerDocument: false,
      }),
    );
    if (spec.viewType === 'offxy.docxEditor') {
      docxProvider = provider;
    }
  }

  // Tear down every open tab's ctl server (TCP listener + discovery file) on
  // extension deactivate. Each panel's own dispose already does this when the
  // user closes a tab; this covers whatever's still open when VS Code shuts
  // the extension host down.
  disposables.push(
    new vscode.Disposable(() => {
      for (const server of activeCtlServers) {
        server.dispose();
      }
      activeCtlServers.clear();
    }),
  );

  // Publish the bundled `mcp/server.mjs` bridge to VS Code's MCP API, so
  // GitHub Copilot's agent mode (and anything else that consumes
  // `vscode.lm`-registered MCP servers) discovers the docxy_*/xlsxy_* tools
  // with no user configuration. This is purely a *pointer* to the script —
  // the server itself is the same dependency-free Node process a `claude mcp
  // add offxy -- node <extension>/mcp/server.mjs` registration would spawn
  // (see the README's "AI assistants" section); it opens no document itself,
  // only bridges to whichever docxy/xlsxy instance (terminal pane or VS Code
  // tab) is already running.
  disposables.push(registerMcpProvider(context));

  if (docxProvider) {
    // Register the command-palette actions once; each posts a bridge command
    // string (tab-delimited) to the active panel's webview.
    const COMMANDS: Array<[string, string]> = [
      ['offxy.toggleBold', 'bold'],
      ['offxy.toggleItalic', 'italic'],
      ['offxy.toggleUnderline', 'underline'],
      ['offxy.toggleStrike', 'strike'],
      ['offxy.heading1', 'heading\t1'],
      ['offxy.heading2', 'heading\t2'],
      ['offxy.heading3', 'heading\t3'],
      ['offxy.normalStyle', 'heading\t0'],
      ['offxy.bulletList', 'list\tbullet'],
      ['offxy.numberedList', 'list\tnumber'],
      ['offxy.alignLeft', 'align\tleft'],
      ['offxy.alignCenter', 'align\tcenter'],
      ['offxy.alignRight', 'align\tright'],
      ['offxy.alignJustify', 'align\tjustify'],
      ['offxy.fontBigger', 'fontsize\t2'],
      ['offxy.fontSmaller', 'fontsize\t-2'],
    ];
    for (const [cmd, op] of COMMANDS) {
      disposables.push(
        vscode.commands.registerCommand(cmd, () => {
          docxProvider!.activePanel?.webview.postMessage({ type: 'command', op });
        }),
      );
    }
    // Replace… prompts for the terms, then drives the engine's replace-all.
    disposables.push(
      vscode.commands.registerCommand('offxy.replace', () => docxProvider!.runReplace()),
    );
    // Markdown ⇄ docx conversion (runs the wasm in the extension host).
    disposables.push(
      vscode.commands.registerCommand('offxy.convertMarkdown', (uri?: vscode.Uri) =>
        convertMarkdownToDocx(context, uri),
      ),
      vscode.commands.registerCommand('offxy.exportMarkdown', () =>
        docxProvider!.runExportMarkdown(context),
      ),
    );
  }

  return vscode.Disposable.from(...disposables);
}

/** Register `mcp/server.mjs` as an MCP server definition provider (VS Code
 *  ≥ 1.101's `vscode.lm.registerMcpServerDefinitionProvider`, paired with
 *  this extension's `contributes.mcpServerDefinitionProviders` entry, id
 *  `"offxy"`). `command`/`args` are built positionally per
 *  `vscode.McpStdioServerDefinition`'s constructor
 *  (`label, command, args?, env?, version?`) — `command: 'node'` rather than
 *  `process.execPath` (the Electron host binary), since running the Electron
 *  binary as plain Node requires `ELECTRON_RUN_AS_NODE` to be set on the
 *  child's environment and VS Code's own samples/docs use `'node'` for
 *  Node-based bundled servers, not `process.execPath`. */
function registerMcpProvider(context: vscode.ExtensionContext): vscode.Disposable {
  const serverPath = vscode.Uri.joinPath(context.extensionUri, 'mcp', 'server.mjs').fsPath;
  const version = String(context.extension.packageJSON?.version ?? '0.0.0');
  return vscode.lm.registerMcpServerDefinitionProvider('offxy', {
    provideMcpServerDefinitions: () => [
      new vscode.McpStdioServerDefinition(
        'Offxy (docxy/xlsxy control bridge)',
        'node',
        [serverPath],
        undefined,
        version,
      ),
    ],
  });
}

/** A live binary document (`.docx`, and eventually other formats). The
 *  authoritative content lives in the webview's wasm session; this object
 *  just holds identity plus the on-disk bytes needed to (re)open, and
 *  coordinates request/response with its webview. */
class BinaryDocument implements vscode.CustomDocument {
  private readonly pending = new Map<number, (value: Uint8Array) => void>();
  private reqSeq = 0;
  /** Set once the editor panel is resolved; used to message the webview. */
  panel?: vscode.WebviewPanel;

  /** This document's agent control-surface server (one per open tab),
   *  constructed in `resolveCustomEditor` and started once the webview's
   *  first `ready` message arrives. */
  ctlServer?: CtlServer;
  /** Guards `start()` against firing twice if `ready` somehow arrives more
   *  than once for the same document. */
  ctlServerStarted = false;
  private readonly ctlPending = new Map<
    number,
    { resolve: (value: string) => void; reject: (reason: Error) => void }
  >();
  private ctlReqSeq = 0;

  constructor(
    public readonly uri: vscode.Uri,
    /** Last-known serialized bytes; replaced when an empty file is seeded with
     *  a fresh document. */
    public initialContent: Uint8Array,
  ) {}

  dispose(): void {
    this.pending.clear();
    this.rejectPendingCtl('document closed');
  }

  /** Ask the webview to serialize the current document and resolve with the
   *  resulting bytes. */
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

  /** Forward one ctl wasm request to the webview (`docx_ctl`/`grid_ctl`'s
   *  marshalling lives there) and resolve with its raw reply JSON string.
   *  Same requestId-map pattern as `requestBytes`. `repaint` rides along on
   *  the message so the webview knows whether to redraw after an ok reply. */
  requestCtl(payload: string, repaint: boolean): Promise<string> {
    const panel = this.panel;
    if (!panel) {
      return Promise.resolve('{"ok":false,"error":"no active webview"}');
    }
    const requestId = ++this.ctlReqSeq;
    return new Promise<string>((resolve, reject) => {
      this.ctlPending.set(requestId, { resolve, reject });
      panel.webview.postMessage({ type: 'ctl', requestId, payload, repaint });
    });
  }

  /** Resolve a pending `requestCtl` call with the reply the webview posted
   *  back as `ctlResult`. */
  fulfillCtl(requestId: number, payload: string): void {
    const pending = this.ctlPending.get(requestId);
    if (pending) {
      this.ctlPending.delete(requestId);
      pending.resolve(payload);
    }
  }

  /** Reject every in-flight `requestCtl` call — called when the tab/document
   *  is going away so a ctl request that was mid-flight (sent to the webview
   *  but never answered) doesn't leave an agent's TCP connection waiting
   *  forever for a reply line that will never arrive. `CtlServer`'s own
   *  `handleLine` try/catch turns a rejection here into a normal
   *  `{"ok":false,"error":…}` reply — this doesn't need to shape one itself. */
  private rejectPendingCtl(reason: string): void {
    for (const pending of this.ctlPending.values()) {
      pending.reject(new Error(reason));
    }
    this.ctlPending.clear();
  }

  /** Stop the ctl server (closes its listener, deletes its discovery file),
   *  settle any request still waiting on this tab's webview, and clear the
   *  reference so a stray late message can't restart it. */
  disposeCtlServer(): void {
    this.rejectPendingCtl('ctl server stopped');
    this.ctlServer?.dispose();
    this.ctlServer = undefined;
    this.ctlServerStarted = false;
  }
}

class OffxyEditorProvider implements vscode.CustomEditorProvider<BinaryDocument> {
  /** The most recently focused panel for this editor, so command-palette
   *  actions target the editor the user is looking at. */
  activePanel?: vscode.WebviewPanel;
  /** Panel → its document, so commands can reach the active document's bytes. */
  private readonly panelDocs = new Map<vscode.WebviewPanel, BinaryDocument>();

  private get activeDocument(): BinaryDocument | undefined {
    return this.activePanel ? this.panelDocs.get(this.activePanel) : undefined;
  }

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly spec: EditorSpec,
  ) {}

  /** Prompt for find/replace terms and apply replace-all in the active editor. */
  async runReplace(): Promise<void> {
    const panel = this.activePanel;
    if (!panel) {
      return;
    }
    const find = await vscode.window.showInputBox({
      title: 'Docxy — Replace',
      prompt: 'Find what',
      ignoreFocusOut: true,
    });
    if (!find) {
      return;
    }
    const withText = await vscode.window.showInputBox({
      title: 'Docxy — Replace',
      prompt: `Replace “${find}” with`,
      ignoreFocusOut: true,
    });
    if (withText === undefined) {
      return; // cancelled (empty string is a valid "delete" replacement)
    }
    panel.webview.postMessage({ type: 'command', op: `replace\t${find}\t${withText}` });
  }

  // --- edit / dirty / undo-redo plumbing ------------------------------------

  private readonly _onDidChange =
    new vscode.EventEmitter<vscode.CustomDocumentEditEvent<BinaryDocument>>();
  readonly onDidChangeCustomDocument = this._onDidChange.event;

  // --- document lifecycle ---------------------------------------------------

  async openCustomDocument(uri: vscode.Uri): Promise<BinaryDocument> {
    const content =
      uri.scheme === 'untitled'
        ? new Uint8Array()
        : await vscode.workspace.fs.readFile(uri);
    return new BinaryDocument(uri, content);
  }

  async resolveCustomEditor(
    document: BinaryDocument,
    panel: vscode.WebviewPanel,
  ): Promise<void> {
    document.panel = panel;
    this.panelDocs.set(panel, document);
    panel.webview.options = {
      enableScripts: true,
      localResourceRoots: [vscode.Uri.joinPath(this.context.extensionUri, 'media')],
    };
    panel.webview.html = this.html(panel.webview);

    // Construct (but don't yet start) this tab's ctl server: every open
    // docx/xlsx tab gets one, so it can be driven by the same terminal
    // agents/MCP clients that talk to a running docxy/xlsxy pane. `start()`
    // happens once the webview confirms it's ready (see `onMessage`) — the
    // listener and discovery file shouldn't come up before there's a session
    // behind them to answer wasm verbs.
    document.ctlServer = new CtlServer(
      this.spec.ctl.app,
      nextCtlSuffix(document.uri),
      this.makeCtlHost(document),
      this.spec.ctl.wasmVerbs,
      this.spec.ctl.mutatingVerbs,
    );

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
      this.panelDocs.delete(panel);
      if (document.ctlServer) {
        activeCtlServers.delete(document.ctlServer);
      }
      document.disposeCtlServer();
      if (document.panel === panel) {
        document.panel = undefined;
      }
      if (this.activePanel === panel) {
        this.activePanel = undefined;
      }
    });
  }

  /** Start `document`'s already-constructed ctl server, once. Called from the
   *  webview's first `ready` message (Step 3 of the wiring: the server has no
   *  live session to answer wasm verbs against before then). */
  private async startCtlServer(document: BinaryDocument): Promise<void> {
    const server = document.ctlServer;
    if (!server || document.ctlServerStarted) {
      return;
    }
    document.ctlServerStarted = true;
    try {
      await server.start();
      activeCtlServers.add(server);
    } catch (err) {
      console.error(`offxy: failed to start the ${this.spec.label} control server:`, err);
    }
  }

  /** Build the `CtlHost` one `CtlServer` uses to answer host verbs
   *  (`${prefix}.path/save/reload/open`) and forward wasm verbs into the live
   *  webview session. One instance per document, closed over `document` so
   *  every callback reaches the right panel even if it's replaced. */
  private makeCtlHost(document: BinaryDocument): CtlHost {
    return {
      callWasm: (requestJson: string) => {
        const verb = ctlVerbOf(requestJson);
        const repaint = verb !== undefined && this.spec.ctl.mutatingVerbs.has(verb);
        return document.requestCtl(requestJson, repaint);
      },

      pathInfo: () => this.fullPathInfo(document),

      save: async () => {
        // The full save pipeline (dirty flag, hot-exit backup cleanup, …),
        // not just a raw file write — `saveCustomDocument` alone wouldn't
        // clear VS Code's dirty indicator.
        await vscode.workspace.save(document.uri);
        return this.fullPathInfo(document);
      },

      reload: async () => {
        const bytes = await vscode.workspace.fs.readFile(document.uri);
        document.initialContent = bytes;
        document.panel?.webview.postMessage({
          type: 'open',
          data: Buffer.from(bytes).toString('base64'),
        });
        // Intentionally fires no `_onDidChange` edit event: this is a revert
        // (drop unsaved edits back to the on-disk file), not an undoable
        // edit. Quirk: unlike VS Code's own "Revert File" command, this does
        // NOT clear VS Code's dirty indicator — there's no public API to do
        // that for a custom document short of the edit-event path, which
        // would wrongly put "reload" on the undo stack.
        return this.fullPathInfo(document);
      },

      open: async (path: string) => {
        // VS Code's per-tab document model has no equivalent to the terminal
        // apps' single mutable "current document" — `${prefix}.open` here
        // opens the target file in its own new tab (its own independent ctl
        // instance) rather than swapping this tab's content.
        await vscode.commands.executeCommand(
          'vscode.openWith',
          vscode.Uri.file(path),
          this.spec.viewType,
        );
        return { path };
      },

      onMutated: (verbLabel: string) => {
        // `doc.replace-range` is a delete-then-insert at the wasm/Editor
        // level (see `docxcore::agent::replace_range` and
        // `docs/agent-control.md`) — two undo checkpoints, not one. Every
        // other mutating verb here (`doc.insert`, `doc.append`, `cell.set`,
        // `range.clear`) checkpoints exactly once. Firing a single VS Code
        // edit event whose undo/redo replays *both* wasm steps keeps a
        // single Ctrl+Z fully reverting one agent action, instead of landing
        // on the intermediate post-delete/pre-insert state.
        const steps =
          this.spec.ctl.app === 'docxy' && verbLabel === 'doc.replace-range' ? 2 : 1;
        this._onDidChange.fire({
          document,
          label: `Agent: ${verbLabel}`,
          undo: () => {
            for (let i = 0; i < steps; i++) {
              void document.panel?.webview.postMessage({ type: 'do', op: 'undo' });
            }
          },
          redo: () => {
            for (let i = 0; i < steps; i++) {
              void document.panel?.webview.postMessage({ type: 'do', op: 'redo' });
            }
          },
        });
      },
    };
  }

  /** Compose the full `${prefix}.path`-shaped reply — the URI-derived half
   *  plus whatever only the live wasm session knows — for `pathInfo()`
   *  (merged again by `CtlServer` with its own `doc.blocks`/`wb.info` call,
   *  harmlessly duplicating a field or two) and for `save()`/`reload()`/
   *  `open()`, which return their result as-is with no such merge. */
  private async fullPathInfo(document: BinaryDocument): Promise<Record<string, unknown>> {
    const filePath = document.uri.fsPath;
    if (this.spec.ctl.app === 'docxy') {
      // `doc.blocks`'s own field is named "total" (it doubles as the count
      // `doc.insert`/`doc.append` report), but `doc.path`'s documented shape
      // calls it "blocks" — relabel here so the reply that actually leaves
      // this process matches `docs/agent-control.md`.
      const blocks = ctlOkResult(
        await document.requestCtl(JSON.stringify({ verb: 'doc.blocks', args: null }), false),
      );
      return {
        path: filePath,
        format: 'docx',
        modified: blocks?.modified ?? false,
        blocks: blocks?.total ?? 0,
      };
    }
    // xlsxy: `wb.path`'s documented shape includes `active_name`, which
    // `wb.info` doesn't carry — pull it from `sheet.list` and merge in.
    const info = ctlOkResult(
      await document.requestCtl(JSON.stringify({ verb: 'wb.info', args: null }), false),
    );
    const list = ctlOkResult(
      await document.requestCtl(JSON.stringify({ verb: 'sheet.list', args: null }), false),
    );
    const sheets = Array.isArray(list?.sheets)
      ? (list!.sheets as Array<{ index: number; name: string }>)
      : [];
    const active = typeof info?.active === 'number' ? (info.active as number) : 0;
    const activeName = sheets.find((s) => s.index === active)?.name ?? '';
    return {
      path: filePath,
      modified: info?.modified ?? false,
      sheets: typeof info?.sheets === 'number' ? (info.sheets as number) : sheets.length,
      active,
      active_name: activeName,
    };
  }

  /** Export the active Docxy document's current content to a sibling `.md`. */
  async runExportMarkdown(context: vscode.ExtensionContext): Promise<void> {
    const document = this.activeDocument;
    if (!document) {
      void vscode.window.showInformationMessage('Docxy: open a .docx first.');
      return;
    }
    const bytes = await document.requestBytes();
    const md = await docxToMarkdown(context, bytes);
    const target = withExtension(document.uri, '.md');
    await vscode.workspace.fs.writeFile(target, new TextEncoder().encode(md));
    const doc = await vscode.workspace.openTextDocument(target);
    await vscode.window.showTextDocument(doc);
    void vscode.window.showInformationMessage(`Docxy: exported ${basename(target)}`);
  }

  /** Send the document bytes to the webview. A 0-byte file (e.g. created via
   *  the explorer's "New File…") can't be parsed as OOXML, so offer to seed it
   *  with a fresh empty document instead. */
  private async openInWebview(
    document: BinaryDocument,
    panel: vscode.WebviewPanel,
  ): Promise<void> {
    if (document.initialContent.length === 0 && this.spec.mintEmpty) {
      const prompt = (this.spec.emptyPrompt ?? '“{name}” is empty. Create a new document in its place?').replace(
        '{name}',
        basename(document.uri),
      );
      const pick = await vscode.window.showInformationMessage(prompt, { modal: true }, 'Create');
      if (pick === 'Create') {
        await this.seedNewDocument(document);
      }
    }
    panel.webview.postMessage({
      type: 'open',
      data: Buffer.from(document.initialContent).toString('base64'),
    });
  }

  /** Mint a fresh empty document and write it over the (empty) file. */
  private async seedNewDocument(document: BinaryDocument): Promise<void> {
    if (!this.spec.mintEmpty) {
      return;
    }
    const bytes = await this.spec.mintEmpty(this.context);
    if (document.uri.scheme !== 'untitled') {
      await vscode.workspace.fs.writeFile(document.uri, bytes);
    }
    document.initialContent = bytes;
  }

  private onMessage(
    document: BinaryDocument,
    panel: vscode.WebviewPanel,
    msg: any,
  ): void {
    switch (msg?.type) {
      case 'ready':
        // Hand the webview the file bytes (base64) to open in wasm.
        void this.openInWebview(document, panel);
        // Bring this tab's ctl server up now that there's a live session
        // behind it to answer wasm verbs against.
        void this.startCtlServer(document);
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

      case 'createNew':
        // The webview's empty-file state: create the document in place.
        void this.seedNewDocument(document).then(() => {
          panel.webview.postMessage({
            type: 'open',
            data: Buffer.from(document.initialContent).toString('base64'),
          });
        });
        break;

      case 'bytes':
        document.fulfillBytes(msg.requestId, new Uint8Array(Buffer.from(msg.data, 'base64')));
        break;

      case 'ctlResult':
        document.fulfillCtl(msg.requestId, msg.payload);
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
    document: BinaryDocument,
    _cancellation: vscode.CancellationToken,
  ): Promise<void> {
    await this.saveAs(document, document.uri);
  }

  async saveCustomDocumentAs(
    document: BinaryDocument,
    destination: vscode.Uri,
    _cancellation: vscode.CancellationToken,
  ): Promise<void> {
    await this.saveAs(document, destination);
  }

  private async saveAs(document: BinaryDocument, target: vscode.Uri): Promise<void> {
    const bytes = await document.requestBytes();
    await vscode.workspace.fs.writeFile(target, bytes);
  }

  async revertCustomDocument(document: BinaryDocument): Promise<void> {
    const bytes = await vscode.workspace.fs.readFile(document.uri);
    document.panel?.webview.postMessage({
      type: 'open',
      data: Buffer.from(bytes).toString('base64'),
    });
  }

  async backupCustomDocument(
    document: BinaryDocument,
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
    const scriptUri = media(this.spec.script);
    const styleUri = media(this.spec.style);
    const wasmUri = media(this.spec.wasm);
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
  <script nonce="${nonce}">window.__OFFXY__ = { wasmUri: "${wasmUri}" };</script>
  <script nonce="${nonce}" src="${scriptUri}"></script>
</body>
</html>`;
  }
}

/** Convert a Markdown file to a sibling `.docx` and open it in the Docxy editor.
 *  `uri` comes from the explorer/editor-title context; falls back to the active
 *  text editor when invoked from the command palette. */
async function convertMarkdownToDocx(
  context: vscode.ExtensionContext,
  uri?: vscode.Uri,
): Promise<void> {
  const source = uri ?? vscode.window.activeTextEditor?.document.uri;
  if (!source || !source.path.toLowerCase().endsWith('.md')) {
    void vscode.window.showInformationMessage('Docxy: select a Markdown (.md) file to convert.');
    return;
  }
  const md = new TextDecoder().decode(await vscode.workspace.fs.readFile(source));
  const docx = await markdownToDocx(context, md);
  const target = withExtension(source, '.docx');
  await vscode.workspace.fs.writeFile(target, docx);
  await vscode.commands.executeCommand('vscode.openWith', target, 'offxy.docxEditor');
  void vscode.window.showInformationMessage(`Docxy: created ${basename(target)}`);
}

/** Replace a URI's file extension (e.g. `report.md` → `report.docx`). */
function withExtension(uri: vscode.Uri, ext: string): vscode.Uri {
  const path = uri.path.replace(/\.[^./]*$/, '') + ext;
  return uri.with({ path });
}

function basename(uri: vscode.Uri): string {
  return uri.path.split('/').pop() || uri.path;
}

function makeNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let s = '';
  for (let i = 0; i < 32; i++) s += chars.charAt(Math.floor(Math.random() * chars.length));
  return s;
}
