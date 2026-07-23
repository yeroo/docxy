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
   *  the open-time modal and the webview's in-tab `createNew` message). These
   *  are webview-native bytes (docx, for both the docx and markdown editors) —
   *  when writing them to disk as a new file's initial content, route through
   *  `toFileBytes` first (see `seedNewDocument`). */
  mintEmpty?: (context: vscode.ExtensionContext) => Promise<Uint8Array>;
  /** Convert on-disk file bytes into the bytes the webview `docx_open`s.
   *  Default identity (the file already holds the editor's native bytes).
   *  The markdown editor uses this to turn `.md` text into in-memory docx
   *  bytes via `markdownToDocx` — no `.docx` file is ever written. */
  fromFileBytes?: (raw: Uint8Array, ctx: vscode.ExtensionContext) => Promise<Uint8Array>;
  /** Convert the webview's saved bytes (docx) back to the on-disk file bytes.
   *  Default identity. The markdown editor uses this to turn the edited docx
   *  model back into markdown text via `docxToMarkdown`. */
  toFileBytes?: (webviewBytes: Uint8Array, ctx: vscode.ExtensionContext) => Promise<Uint8Array>;
  /** When true, the webview runs in markdown mode (constrained toolbar +
   *  checkbox rendering); passed to the webview via `window.__OFFXY__`. */
  markdown?: boolean;
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

/** The docx editor's agent control-surface identity (see `EditorSpec.ctl`),
 *  hoisted out of `EDITORS` so the markdown editor spec below can clone it
 *  (new `Set` instances) without a self-referential `EDITORS[0].ctl` read
 *  during `EDITORS`'s own initializer — `EDITORS` isn't assigned yet at that
 *  point (TDZ), so that read would throw at module-load time. */
const DOCX_CTL: EditorSpec['ctl'] = {
  app: 'docxy',
  // 'doc.blocks' is deliberately NOT in this set: it's an internal-only
  // verb `fullPathInfo()` calls directly (bypassing this gate), used to
  // compose `doc.path`'s reply. Terminal docxy's control.rs has no
  // `doc.blocks` arm, so exposing it here to external agents would let a
  // VS Code tab answer a verb a terminal instance rejects as "unknown
  // verb" — breaking "indistinguishable from a terminal instance".
  //
  // 'doc.export-pdf' is also deliberately absent: `CtlServer` special-cases
  // it (host-assisted write; see `CtlServer.exportPdf`) ahead of this gate,
  // so it's reachable without being listed here — listing it would route it
  // through the plain wasm pass-through, which returns internal `pdfBase64`
  // instead of writing the file.
  wasmVerbs: new Set([
    'doc.outline',
    'doc.read',
    'doc.find',
    'doc.replace-range',
    'doc.insert',
    'doc.append',
    // Wave-1 read verbs (no repaint, no edit event).
    'doc.export',
    'doc.comments',
    'doc.notes',
    'doc.header',
    'doc.footer',
    'doc.metadata',
    'doc.stats',
    // Wave-1 mutating verbs.
    'doc.replace-all',
    'doc.undo',
    'doc.redo',
    // Wave-3 mutating verbs (block-range formatting).
    'doc.format',
    'doc.set-style',
  ]),
  mutatingVerbs: new Set([
    'doc.replace-range',
    'doc.insert',
    'doc.append',
    // 'doc.replace-all' fires an edit event only when it actually replaced
    // something: `handleLine`'s no-op gate skips firing when undoSteps is 0
    // (a zero-match run — the wasm leaves no undo checkpoint), so no dead
    // undo entry lands and the doc isn't dirtied. A real replace (undoSteps
    // >0) takes the bucket-A `undoSteps` replay path in `onMutated`.
    // 'doc.undo'/'redo' take the inverse-wasm-op adaptation (onMutated
    // Case 1); `handleLine` skips firing them entirely when `{done:false}`.
    'doc.replace-all',
    'doc.undo',
    'doc.redo',
    // Wave-3: doc.format/doc.set-style land ONE true wasm-undo-stack
    // checkpoint each (agent::format_range/set_style_range push exactly
    // one, same mechanism doc.insert/append/replace-all use) — bucket A,
    // default steps=1 (neither carries an undoSteps field on the wire;
    // see docxwasm's ctl_format/ctl_set_style doc comments).
    'doc.format',
    'doc.set-style',
  ]),
};

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
    ctl: DOCX_CTL,
  },
  {
    viewType: 'offxy.markdownEditor',
    label: 'Markdown document',
    script: 'webview.js',
    style: 'webview.css',
    wasm: 'docxwasm.wasm',
    markdown: true,
    emptyPrompt:
      '“{name}” is empty. Start a new Markdown document here?',
    // Empty md file → an empty docx model for the webview.
    mintEmpty: (ctx) => markdownToDocx(ctx, ''),
    // `.md` text on disk  <->  in-memory docx bytes for the webview.
    fromFileBytes: (raw, ctx) => markdownToDocx(ctx, new TextDecoder().decode(raw)),
    toFileBytes: async (bytes, ctx) => new TextEncoder().encode(await docxToMarkdown(ctx, bytes)),
    // Same docxy control surface as the docx editor (below), but with its own
    // `Set` instances — the two editors must not share mutable Set references.
    ctl: {
      app: 'docxy',
      wasmVerbs: new Set(DOCX_CTL.wasmVerbs),
      mutatingVerbs: new Set(DOCX_CTL.mutatingVerbs),
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
      //
      // 'sheet.restore-removed' is also deliberately absent: it's an
      // INTERNAL-only gridwasm verb reached ONLY through a `sheet.remove`
      // edit event's host-orchestrated inverse (see `onMutated`). Terminal
      // xlsxy has no such verb, so an external agent calling it directly must
      // get "unknown verb" — which omitting it from this allow-list delivers
      // for free (the inverse path calls the webview session directly,
      // bypassing this gate).
      wasmVerbs: new Set([
        'sheet.list',
        'sheet.read',
        'cell.get',
        'cell.set',
        'range.clear',
        'find',
        'wb.recalc',
        // Wave-1 read verbs (no repaint, no edit event).
        'comment.list',
        'wb.export-csv',
        'sheet.pivot',
        'formula.eval',
        'sheet.stats',
        'chart.list',
        'pivot.list',
        // Wave-1 mutating verbs.
        'comment.add',
        'comment.remove',
        'range.set',
        'sheet.import-csv',
        'wb.replace-all',
        'sheet.add',
        'sheet.remove',
        'sheet.rename',
        'row.insert',
        'row.delete',
        'col.insert',
        'col.delete',
        // Wave-2 mutating verbs (cell formatting + column width).
        'cell.format',
        'col.width',
        // Wave-3 mutating verb (persistent pivot tables).
        'pivot.create',
      ]),
      mutatingVerbs: new Set([
        'cell.set',
        'range.clear',
        // Bucket A — one true wasm-undo-stack entry (undoSteps replay):
        'range.set',
        'sheet.rename',
        'row.insert',
        'row.delete',
        'col.insert',
        'col.delete',
        'wb.replace-all',
        'sheet.add',
        // Wave-2: cell.format lands on the SAME true wasm-undo-stack group
        // range.set uses (Session::apply captures the style-only before/after
        // Cell diff) — bucket A, undoSteps:1 (see gridwasm's ctl_cell_format).
        'cell.format',
        // Bucket B — host-orchestrated inverse (comment add ⇄ remove):
        'comment.add',
        'comment.remove',
        // Wave-2: col.width is NOT on xlsxy's own undo stack (mirrors the
        // TUI's F7/F8 width-adjust path); the wasm reply carries an internal
        // `inverse` (a col.width call restoring the prior width) that this
        // same bucket-B flip-flop mechanism drives as the edit event's
        // undo()/redo() — undoSteps:0.
        'col.width',
        // Bucket C — history-cleared + inverse (import-csv ⇄ remove;
        // remove ⇄ restore-removed):
        'sheet.import-csv',
        'sheet.remove',
        // Wave-3: pivot.create lands a new sheet + pivot-part registration —
        // not a cell-level change the wasm undo stack can invert, so it
        // clears history like sheet.import-csv (same bucket-C mechanism,
        // undoSteps:0). Its declared inverse is sheet.remove on the newly
        // created sheet (already in this same bucket above); remove_sheet's
        // pivot cascade drops the pivot registration WITH the sheet, so the
        // existing inverse flip-flop (sheet.remove ⇄ sheet.restore-removed)
        // round-trips the pivot for free — no pivot-specific host code
        // needed (see gridwasm's ctl_pivot_create doc comment).
        'pivot.create',
      ]),
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

/** Build a `CtlServer` instance suffix: a filesystem-safe basename, this
 *  extension host's process id, and a per-session sequence number. The pid is
 *  essential for cross-window uniqueness: the sequence counter is per
 *  extension host, so two VS Code windows each opening a same-basename file
 *  would otherwise mint identical instance ids (`report_docx-1` in both) and
 *  clobber each other's discovery file — the loser's 30s refresh only checks
 *  existence, so it never recovers. Interleaving the pid keeps the ids
 *  distinct across windows. */
function nextCtlSuffix(uri: vscode.Uri): string {
  const safe = basename(uri).replace(/[^A-Za-z0-9._-]/g, '_') || 'doc';
  return `${safe}-${process.pid}-${++ctlInstanceSeq}`;
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

  /** Convert on-disk bytes into the webview-native bytes `document.initialContent`
   *  holds (identity unless `spec.fromFileBytes` is set). A genuinely empty file
   *  (0 bytes — the "new file" case) is passed through as-is, NOT run through
   *  the transform: `openInWebview`'s empty-file detection
   *  (`document.initialContent.length === 0`) drives the create/mint prompt, and
   *  `markdownToDocx(ctx, '')` produces non-empty docx bytes — running it here
   *  unconditionally would hide every empty `.md` from that prompt. */
  private async fromDisk(raw: Uint8Array): Promise<Uint8Array> {
    if (raw.length === 0 || !this.spec.fromFileBytes) {
      return raw;
    }
    return this.spec.fromFileBytes(raw, this.context);
  }

  /** Convert the webview's saved bytes (always webview-native, e.g. docx) into
   *  the bytes actually written to disk (identity unless `spec.toFileBytes` is
   *  set — the markdown editor's docx-bytes-to-markdown-text conversion). */
  private async toDisk(bytes: Uint8Array): Promise<Uint8Array> {
    return this.spec.toFileBytes ? this.spec.toFileBytes(bytes, this.context) : bytes;
  }

  async openCustomDocument(uri: vscode.Uri): Promise<BinaryDocument> {
    const raw =
      uri.scheme === 'untitled'
        ? new Uint8Array()
        : await vscode.workspace.fs.readFile(uri);
    const content = await this.fromDisk(raw);
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
        const raw = await vscode.workspace.fs.readFile(document.uri);
        const bytes = await this.fromDisk(raw);
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

      onMutated: (
        verbLabel: string,
        undoSteps?: number,
        inverse?: { verb: string; args: unknown },
      ) => {
        // Three distinct edit-event shapes, one per undo-mechanism bucket the
        // wasm layer verified per verb (Tasks 3/5/6). `CtlServer` has already
        // stripped the internal `undoSteps`/`inverse` fields off the wire and
        // handed them here.

        // --- Case 1: docxy `doc.undo` / `doc.redo` — inverse-wasm-op event. --
        // The agent's `doc.undo` ALREADY ran the wasm undo; there's no new wasm
        // undo-stack entry to replay. So we register a NEW VS Code edit event
        // whose own undo/redo drives the INVERSE wasm op, keeping the two
        // stacks in lockstep. Truth table (the agent verb's wasm op is what
        // already happened; the event's undo() must REVERSE it):
        //
        //   agent verb | wasm op that ran | event.undo() sends | event.redo() sends
        //   -----------+------------------+--------------------+-------------------
        //   doc.undo   | wasm undo        | wasm redo (reverse)| wasm undo (replay)
        //   doc.redo   | wasm redo        | wasm undo (reverse)| wasm redo (replay)
        //
        // (`handleLine` only reaches here for `{done:true}`, so the wasm op
        // genuinely happened; a `{done:false}` no-op fires no event.)
        if (
          this.spec.ctl.app === 'docxy' &&
          (verbLabel === 'doc.undo' || verbLabel === 'doc.redo')
        ) {
          const reverseOp = verbLabel === 'doc.undo' ? 'redo' : 'undo';
          const replayOp = verbLabel === 'doc.undo' ? 'undo' : 'redo';
          this._onDidChange.fire({
            document,
            label: `Agent: ${verbLabel === 'doc.undo' ? 'undo' : 'redo'}`,
            undo: () => {
              void document.panel?.webview.postMessage({ type: 'do', op: reverseOp });
            },
            redo: () => {
              void document.panel?.webview.postMessage({ type: 'do', op: replayOp });
            },
          });
          return;
        }

        // --- Case 2: host-orchestrated inverse (buckets B/C). ---------------
        // The change isn't on the wasm undo stack (comment parts / package
        // parts live outside it), so the wasm reply carried an `inverse` ctl
        // request that reverses it. We drive that inverse into the webview as
        // the event's undo(). Each inverse call's OWN reply carries the
        // inverse-of-the-inverse (the op that re-does the change), so we
        // flip-flop `undoReq`/`redoReq` across repeated undo/redo without ever
        // needing the original call's args here. This self-heals the
        // import-csv/remove/restore chain: undoing a `sheet.import-csv` sends
        // `sheet.remove`, whose reply's inverse is `sheet.restore-removed`
        // (which brings the sheet back WITH its data from the bridge stash),
        // so redo restores content, not an empty sheet.
        if (inverse) {
          let undoReq: { verb: string; args: unknown } | undefined = inverse;
          let redoReq: { verb: string; args: unknown } | undefined;
          this._onDidChange.fire({
            document,
            label: `Agent: ${verbLabel}`,
            undo: async () => {
              if (!undoReq) {
                void vscode.window.showWarningMessage(
                  `Offxy: couldn't undo “${verbLabel}” — nothing left to reverse.`,
                );
                return;
              }
              const res = await this.applyInverseRequest(document, undoReq);
              if (!res.ok) {
                // A failed inverse (e.g. the single-slot sheet-restore stash
                // already consumed by an earlier undo — the disclosed
                // double-remove/double-undo failure mode) must be SURFACED, not
                // swallowed, and must NOT throw: VS Code's undo pointer advances
                // regardless, so a silent failure would lose data invisibly.
                void vscode.window.showWarningMessage(
                  `Offxy: couldn't undo “${verbLabel}”: ${res.error}`,
                );
                return;
              }
              redoReq = res.inverse; // re-doing = reversing what we just undid
              undoReq = undefined;
            },
            redo: async () => {
              if (!redoReq) {
                void vscode.window.showWarningMessage(
                  `Offxy: couldn't redo “${verbLabel}” — nothing to reapply.`,
                );
                return;
              }
              const res = await this.applyInverseRequest(document, redoReq);
              if (!res.ok) {
                void vscode.window.showWarningMessage(
                  `Offxy: couldn't redo “${verbLabel}”: ${res.error}`,
                );
                return;
              }
              undoReq = res.inverse;
              redoReq = undefined;
            },
          });
          return;
        }

        // --- Case 3 (default): wasm-undo-stack replay (bucket A). -----------
        // Replay exactly the number of wasm undo checkpoints the edit pushed.
        // `doc.replace-range` reports 2 (a delete-then-insert; 1 for a single
        // empty paragraph — hard-coding 2 would over-unwind and destroy a prior
        // edit, see `docxcore::agent::replace_range`); every other bucket-A
        // verb (`doc.insert`/`append`/`replace-all`, `range.set`,
        // `sheet.rename`/`add`, `row.*`/`col.*`, `wb.replace-all`, and the
        // legacy `cell.set`/`range.clear`) checkpoints once. One VS Code edit
        // event replaying exactly `steps` wasm steps keeps one Ctrl+Z fully
        // reverting one agent action.
        const steps = undoSteps ?? 1;
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

  /** Drive one host-orchestrated inverse ctl request into `document`'s webview
   *  session (the undo/redo of a bucket-B/C edit event; see `onMutated` Case
   *  2), repainting after. Returns `{ok:true, inverse}` with the reply's own
   *  inverse-of-the-inverse (the request that re-does what this one just
   *  reversed) for the flip-flop, or `{ok:false, error}` so the caller can
   *  surface a warning without throwing. Verbs reached this way (including the
   *  internal-only `sheet.restore-removed`) bypass `CtlServer`'s `wasmVerbs`
   *  gate — they go straight to the live session — which is exactly why
   *  `sheet.restore-removed` stays off the external allow-list yet still works
   *  here. */
  private async applyInverseRequest(
    document: BinaryDocument,
    req: { verb: string; args: unknown },
  ): Promise<{ ok: boolean; inverse?: { verb: string; args: unknown }; error?: string }> {
    const raw = await document.requestCtl(
      JSON.stringify({ verb: req.verb, args: req.args ?? null }),
      true,
    );
    let reply: Record<string, unknown>;
    try {
      reply = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      return { ok: false, error: 'the document returned a malformed reply' };
    }
    if (reply.ok !== true) {
      return {
        ok: false,
        error: typeof reply.error === 'string' ? reply.error : 'the operation failed',
      };
    }
    const inv = reply.inverse;
    const inverse =
      inv && typeof inv === 'object' && typeof (inv as { verb?: unknown }).verb === 'string'
        ? (inv as { verb: string; args: unknown })
        : undefined;
    return { ok: true, inverse };
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
      const info: Record<string, unknown> = {
        path: filePath,
        format: 'docx',
        modified: blocks?.modified ?? false,
        blocks: blocks?.total ?? 0,
      };
      // `doc.path`'s present-if-set `protection`/`watermark` keys — carried by
      // ALL of terminal docxy's path-shaped replies (`doc.save`/`reload`/`open`
      // return the full `path_info`, control.rs), so a tab's must too. `doc.path`
      // itself also gets them via `CtlServer.resolvePathInfo`'s allowlist, but
      // save/reload/open return this object as-is with no such merge — so add
      // them here (present-if-set, exactly as `doc.blocks` reports them).
      for (const key of ['protection', 'watermark'] as const) {
        if (blocks && key in blocks) {
          info[key] = blocks[key];
        }
      }
      return info;
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

  /** Mint a fresh empty document and write it over the (empty) file.
   *  `mintEmpty` always returns webview-native bytes (docx) — `document.initialContent`
   *  takes those directly (it's what `openInWebview` hands the webview), but the
   *  on-disk write goes through `toDisk` so the markdown editor writes an empty
   *  `.md` (not docx bytes) as the new file's content. */
  private async seedNewDocument(document: BinaryDocument): Promise<void> {
    if (!this.spec.mintEmpty) {
      return;
    }
    const bytes = await this.spec.mintEmpty(this.context);
    if (document.uri.scheme !== 'untitled') {
      await vscode.workspace.fs.writeFile(document.uri, await this.toDisk(bytes));
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
    await vscode.workspace.fs.writeFile(target, await this.toDisk(bytes));
  }

  async revertCustomDocument(document: BinaryDocument): Promise<void> {
    const raw = await vscode.workspace.fs.readFile(document.uri);
    const bytes = await this.fromDisk(raw);
    document.initialContent = bytes;
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
    await vscode.workspace.fs.writeFile(ctx.destination, await this.toDisk(bytes));
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
    // Only the docx/markdown editor (webview.js) overlays Mermaid diagrams —
    // the grid editor (grid.js) never references `mermaid`, so it skips the
    // ~3MB script tag entirely.
    const hasMermaid = this.spec.script === 'webview.js';
    const mermaidUri = hasMermaid ? media('mermaid.min.js') : null;
    const nonce = makeNonce();
    const csp = [
      `default-src 'none'`,
      `img-src ${webview.cspSource} data:`,
      // 'unsafe-inline' here is Mermaid's, not ours: mermaid.render() injects
      // a <style> element (per-diagram CSS) directly into the DOM at render
      // time, with no nonce attached (it's the UMD bundle's own DOM call, not
      // one of our <script>/<style> tags) — without 'unsafe-inline' the
      // browser drops that stylesheet and every live-rendered diagram comes
      // out unstyled (default black-on-white, no theme colors/fonts). Every
      // other directive stays nonce/host-scoped; only style-src widens.
      `style-src ${webview.cspSource} 'unsafe-inline'`,
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
  <script nonce="${nonce}">window.__OFFXY__ = { wasmUri: "${wasmUri}", markdown: ${this.spec.markdown === true} };</script>
  ${mermaidUri ? `<script nonce="${nonce}" src="${mermaidUri}"></script>` : ''}
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
