// ctlserver.ts — extension-host control server: a Node port of ctlcore's
// loopback TCP control surface (see docs/agent-control.md and
// ctlcore/src/lib.rs), so a VS Code tab can be driven by the same terminal
// agents/MCP clients that already speak to docxy/xlsxy panes. Wire framing
// and the discovery-file shape are byte-compatible with ctlcore; what
// differs is the transport (Node `net` instead of std::net) and who answers
// a verb, since here the live document lives in a webview rather than this
// process — wasm verbs round-trip through `CtlHost.callWasm`, host verbs
// (save/reload/open/path) are answered directly.
//
// No runtime dependencies: only Node built-ins, per the extension's
// zero-dependency bundling policy.

import * as crypto from 'crypto';
import * as fs from 'fs';
import * as net from 'net';
import * as os from 'os';
import * as path from 'path';
import { StringDecoder } from 'string_decoder';

/** What a `CtlServer` needs from its embedding provider to answer requests.
 *  One implementation per open document (docx/xlsx), wired up in the
 *  `CustomEditorProvider` (Task 5). */
export interface CtlHost {
  /** Forward one wasm control verb — `{"verb":…,"args":…}`, no token/id — into
   *  the live webview session and resolve with its raw reply JSON string:
   *  docxwasm/gridwasm's flat `{...fields,"ok":true}` /
   *  `{"ok":false,"error":"…"}` envelope (see their `ctl_ok`/`ctl_err`). */
  callWasm(requestJson: string): Promise<string>;
  /** `doc.path`/`wb.path`'s host-known half (URI, format, …) — the server
   *  merges this with a `doc.blocks`/`wb.info` wasm call for the
   *  block-count/modified fields only the live session knows. */
  pathInfo(): Promise<object>;
  save(): Promise<object>;
  reload(): Promise<object>;
  open(path: string): Promise<object>;
  /** Fired once, right after a mutating verb's successful reply is computed,
   *  so the provider can raise the VS Code custom-document edit event (dirty
   *  dot + undo lockstep). Not called for read-only or failed verbs, nor for a
   *  no-op `doc.undo`/`doc.redo` (`{done:false}` — nothing on the stack).
   *
   *  `undoSteps` is how many native wasm undo checkpoints the edit pushed — the
   *  provider replays exactly this many wasm undos per one VS Code undo for the
   *  wasm-undo-stack bucket, or the two stacks desync. `doc.replace-range`
   *  reports 2 (a delete-then-insert; 1 when the range was a single empty
   *  paragraph); the wave-1 grid mutators (`range.set`, `sheet.rename`,
   *  `row.*`/`col.*`, `wb.replace-all`, `sheet.add`) and the docx ones
   *  (`doc.insert`/`append`/`replace-all`) checkpoint once; the inverse-op
   *  bucket reports 0. Defaults to 1 when absent.
   *
   *  `inverse` is present only for verbs whose change is NOT on the wasm undo
   *  stack (comment.add ⇄ comment.remove; sheet.import-csv / sheet.remove,
   *  whose inverses are `sheet.remove` / `sheet.restore-removed`). It is the
   *  ctl request that reverses this op; the provider drives it into the webview
   *  as the edit event's undo(). `CtlServer` strips it off the wire (like
   *  `undoSteps`) — it must never reach an external agent. */
  onMutated(
    verbLabel: string,
    undoSteps?: number,
    inverse?: { verb: string; args: unknown },
  ): void;
}

/** The only keys `resolvePathInfo` may introduce into `doc.path`/`wb.path`
 *  from the wasm `doc.blocks`/`wb.info` reply beyond the ones `host.pathInfo()`
 *  already declares: `doc.path`'s present-if-set `protection`/`watermark`
 *  (docxy only; `wb.info` never carries them, so the guard is a harmless no-op
 *  for xlsxy). Deliberately narrow — see `resolvePathInfo` for why a blanket
 *  spread of the wasm reply would leak undocumented fields onto the wire. */
const PATHINFO_MERGE_ALLOWLIST = ['protection', 'watermark'] as const;

/** One control-surface instance's discovery record (`docs/agent-control.md`
 *  → "Discovery"), written to `<ctlDir>/<instance>.json`. */
interface Discovery {
  instance: string;
  port: number;
  token: string;
  pid: number;
}

/** The directory an app publishes its control discovery files into:
 *  `%APPDATA%\<app>\ctl` on Windows, `$XDG_CONFIG_HOME/<app>/ctl` (falling
 *  back to `~/.config/<app>/ctl`) elsewhere — mirrors
 *  `ctlcore::config_ctl_dir`. */
function ctlDir(app: string): string {
  const base =
    process.platform === 'win32'
      ? (process.env.APPDATA ?? path.join(os.homedir(), 'AppData', 'Roaming'))
      : (process.env.XDG_CONFIG_HOME ?? path.join(os.homedir(), '.config'));
  return path.join(base, app, 'ctl');
}

/** Build one reply line: `{"ok":true,"result":…,"id":…}` or
 *  `{"ok":false,"error":…,"id":…}` — `id` included only when the request
 *  carried one. Byte-compatible with `ctlcore::Reply::to_line`. */
function frame(ok: boolean, payload: unknown, id: unknown): string {
  const out: Record<string, unknown> = { ok };
  if (ok) {
    out.result = payload;
  } else {
    out.error = payload;
  }
  if (id !== undefined) {
    out.id = id;
  }
  return JSON.stringify(out) + '\n';
}

/** A ctlcore-compatible control server bound to one open document. Each open
 *  custom-editor tab owns one instance; `start()` opens the loopback listener
 *  and writes the discovery file, `dispose()` tears both down. Format-neutral:
 *  the wasm verb set and the mutating-verb set are constructor parameters, so
 *  the same class serves both docx and xlsx tabs. */
export class CtlServer {
  readonly instance: string;
  /** `doc` for docxy, `wb` for xlsxy — the host-verb prefix. */
  private readonly prefix: 'doc' | 'wb';
  private readonly discoveryPath: string;
  private server?: net.Server;
  private token = '';
  private port = 0;
  private refreshTimer?: ReturnType<typeof setInterval>;
  /** Single in-flight request per server: every request's handling chains
   *  onto this promise so requests across *all* connections serialize,
   *  mirroring the terminal's single-threaded consumer. */
  private queue: Promise<void> = Promise.resolve();
  /** The undo-checkpoint count reported by the most recent wasm call, stripped
   *  from the wire result and stashed here for `handleLine` to hand `onMutated`
   *  (see `callWasm`). Safe as a single field: requests serialize through
   *  `queue`, and `onMutated` runs synchronously right after the call that
   *  sets it, with no intervening wasm call. */
  private lastUndoSteps = 1;
  /** The host-orchestrated `inverse` request the most recent wasm call
   *  reported (buckets B/C: comment add/remove, sheet import-csv/remove),
   *  stripped from the wire and stashed here for `handleLine` to hand
   *  `onMutated`. Same single-field safety as `lastUndoSteps`: requests
   *  serialize through `queue`, and `onMutated` runs synchronously right after
   *  the call that sets it. Reset to `undefined` on every wasm call so a later
   *  non-reporting verb can't inherit a stale inverse. */
  private lastInverse: { verb: string; args: unknown } | undefined;

  constructor(
    private readonly app: 'docxy' | 'xlsxy',
    instanceSuffix: string,
    private readonly host: CtlHost,
    private readonly wasmVerbs: Set<string>,
    private readonly mutatingVerbs: Set<string>,
    private readonly refreshMs = 30_000,
  ) {
    this.instance = `${app}-vscode-${instanceSuffix}`;
    this.prefix = app === 'docxy' ? 'doc' : 'wb';
    this.discoveryPath = path.join(ctlDir(app), `${this.instance}.json`);
  }

  /** Start listening on an OS-assigned loopback port and publish the
   *  discovery file; resolves once both are done. */
  async start(): Promise<void> {
    this.token = crypto.randomBytes(24).toString('hex');
    const server = net.createServer((socket) => this.handleConnection(socket));
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject);
      server.listen(0, '127.0.0.1', () => {
        server.removeListener('error', reject);
        resolve();
      });
    });
    // Connection-level errors (client resets, etc.) are handled per-socket;
    // this just keeps a stray post-listen server error from crashing the host.
    server.on('error', () => undefined);
    const addr = server.address();
    this.port = typeof addr === 'object' && addr !== null ? addr.port : 0;
    this.server = server;

    this.writeDiscovery();
    this.refreshTimer = setInterval(() => {
      // Terminal docxy/xlsxy instances sweep stale discovery files on their
      // own startup; a sweep could catch ours between refreshes (or a user
      // could delete it by hand). Restore it so agents keep finding us.
      if (!fs.existsSync(this.discoveryPath)) {
        this.writeDiscovery();
      }
    }, this.refreshMs);
  }

  /** Close the listener, delete the discovery file, and stop the refresh timer. */
  dispose(): void {
    if (this.refreshTimer) {
      clearInterval(this.refreshTimer);
      this.refreshTimer = undefined;
    }
    this.server?.close();
    this.server = undefined;
    try {
      fs.unlinkSync(this.discoveryPath);
    } catch {
      /* already gone */
    }
  }

  // --- discovery file -----------------------------------------------------

  private writeDiscovery(): void {
    const dir = path.dirname(this.discoveryPath);
    fs.mkdirSync(dir, { recursive: true });
    const record: Discovery = {
      instance: this.instance,
      port: this.port,
      token: this.token,
      pid: process.pid,
    };
    // Write to a temp sibling then rename, so a reader never observes a
    // half-written file — same approach as `ctlcore::serve`.
    const tmp = path.join(dir, `${this.instance}.json.${process.pid}.tmp`);
    fs.writeFileSync(tmp, JSON.stringify(record));
    fs.renameSync(tmp, this.discoveryPath);
  }

  // --- connection / line handling ------------------------------------------

  private handleConnection(socket: net.Socket): void {
    let buffer = '';
    // A stateful decoder, not `chunk.toString('utf8')` per chunk: TCP can
    // split a multi-byte UTF-8 character (é, €, emoji, CJK) across two
    // `data` events. Decoding each chunk independently would turn the
    // straddling bytes into U+FFFD replacement characters — silent text
    // corruption on large doc.insert/append/replace-range payloads (the
    // JSON itself still parses fine, so nothing else catches it).
    // `StringDecoder` holds back incomplete trailing byte sequences across
    // `write` calls until enough bytes arrive to complete the character.
    const decoder = new StringDecoder('utf8');
    socket.on('data', (chunk: Buffer) => {
      buffer += decoder.write(chunk);
      let idx: number;
      while ((idx = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, idx).trim();
        buffer = buffer.slice(idx + 1);
        if (line.length === 0) {
          continue; // ctlcore skips blank lines silently, no reply
        }
        this.enqueue(line, socket);
      }
    });
    socket.on('error', () => undefined); // client went away mid-write
  }

  /** Chain one line's handling onto the server-wide queue so requests across
   *  every connection serialize, then write the reply back to its socket. */
  private enqueue(line: string, socket: net.Socket): void {
    const run = this.queue.then(() => this.handleLine(line));
    this.queue = run.then(
      () => undefined,
      () => undefined,
    );
    void run.then((replyLine) => {
      if (!socket.destroyed) {
        socket.write(replyLine);
      }
    });
  }

  /** Parse, authenticate, and route one request line; never throws — every
   *  failure becomes an `{"ok":false,...}` reply line. */
  private async handleLine(line: string): Promise<string> {
    let msg: Record<string, unknown>;
    try {
      msg = JSON.parse(line);
    } catch (e) {
      // No id to echo: the line didn't parse, so there's nothing to read one
      // from. Mirrors ctlcore's `dispatch_line` (`invalid json: {e}`).
      return frame(false, `invalid json: ${(e as Error).message}`, undefined);
    }
    const rawId = msg?.id;
    const id = rawId !== undefined && rawId !== null ? rawId : undefined;

    if (typeof msg?.token !== 'string' || msg.token !== this.token) {
      return frame(false, 'unauthorized: bad or missing token', id);
    }
    const verb = msg.verb;
    if (typeof verb !== 'string') {
      return frame(false, 'missing verb', id);
    }
    const args = msg.args ?? null;

    try {
      const result = await this.dispatch(verb, args);
      if (this.mutatingVerbs.has(verb)) {
        // Fire the edit event (dirty dot + undo entry) only when the verb
        // actually changed the document — a no-op mutating call must neither
        // dirty the doc nor land a dead entry on VS Code's undo stack:
        //  - `doc.undo`/`doc.redo` report `{done:bool}`; fire only on
        //    `done:true` (a real stack pop). `done:false` = nothing to undo.
        //  - wasm-undo-stack verbs push a checkpoint when they change
        //    something (`undoSteps>0`); a zero-match `doc.replace-all` or an
        //    empty-batch `range.set` reports `undoSteps:0` and leaves no wasm
        //    tracks — skip it.
        //  - inverse-op verbs (comment/sheet add/remove/import) report
        //    `undoSteps:0` but a REAL change, marked by carrying an `inverse`;
        //    a `comment.remove` that found nothing carries none, so it skips.
        const isUndoRedo = verb === 'doc.undo' || verb === 'doc.redo';
        const done = (result as { done?: unknown } | null)?.done;
        const realChange = isUndoRedo
          ? done === true
          : this.lastUndoSteps > 0 || this.lastInverse !== undefined;
        if (realChange) {
          this.host.onMutated(verb, this.lastUndoSteps, this.lastInverse);
        }
      }
      return frame(true, result, id);
    } catch (e) {
      return frame(false, e instanceof Error ? e.message : String(e), id);
    }
  }

  // --- verb routing ---------------------------------------------------------

  private async dispatch(verb: string, args: unknown): Promise<unknown> {
    if (verb === `${this.prefix}.path`) {
      return this.resolvePathInfo();
    }
    if (verb === `${this.prefix}.save`) {
      return this.host.save();
    }
    if (verb === `${this.prefix}.reload`) {
      return this.host.reload();
    }
    if (verb === `${this.prefix}.open`) {
      const p = (args as { path?: unknown } | null)?.path;
      if (typeof p !== 'string') {
        throw new Error(`${this.prefix}.open needs a 'path' string`);
      }
      return this.host.open(p);
    }
    if (this.app === 'docxy' && verb === 'doc.export-pdf') {
      return this.exportPdf(args);
    }
    if (this.wasmVerbs.has(verb)) {
      return this.callWasm(verb, args);
    }
    throw new Error(`unknown verb '${verb}'`);
  }

  /** `doc.export-pdf` (docxy tabs), host-assisted. The wasm exporter is
   *  std-only and can't touch the filesystem, so its ctl verb takes no `path`
   *  and returns the rendered PDF as base64 (`pdfBase64`, an internal field);
   *  the extension host validates the target path, decodes the bytes, and
   *  writes the file — landing on the SAME wire reply (`{path}`) a terminal
   *  docxy's direct write produces. The error family
   *  (`bad path:`/`already exists:`/`create failed:`) is byte-identical to
   *  `server.mjs`'s `doNew` (and thus to `ctlcore::client::new_file`): empty
   *  path, existing target, and creation failures all read the same on every
   *  surface. Precedence matches terminal `export_pdf` (control.rs): missing
   *  path → bad path → already-exists → render → exclusive create. */
  private async exportPdf(args: unknown): Promise<object> {
    const p = (args as { path?: unknown } | null)?.path;
    if (typeof p !== 'string') {
      throw new Error("doc.export-pdf needs a 'path'");
    }
    // `path.resolve('')` would fall back to cwd, but Rust's
    // `std::path::absolute("")` errors — match that (and `doNew`) explicitly.
    if (p === '') {
      throw new Error('bad path: cannot make an empty path absolute');
    }
    const abs = path.resolve(p);
    if (fs.existsSync(abs)) {
      throw new Error(`already exists: ${abs}`);
    }
    // Render only after the path is known-writable (mirrors the terminal's
    // precedence). The wasm reply is docxwasm's flat `{pdfBase64,ok:true}`.
    const raw = await this.host.callWasm(JSON.stringify({ verb: 'doc.export-pdf', args: {} }));
    const reply = JSON.parse(raw) as Record<string, unknown>;
    if (reply.ok !== true) {
      throw new Error(typeof reply.error === 'string' ? reply.error : 'wasm call failed');
    }
    if (typeof reply.pdfBase64 !== 'string') {
      throw new Error('create failed: the document engine returned no PDF data');
    }
    const pdf = Buffer.from(reply.pdfBase64, 'base64');
    try {
      fs.mkdirSync(path.dirname(abs), { recursive: true });
    } catch (e) {
      throw new Error(`create failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    try {
      // 'wx' = exclusive create: a file appearing between the exists check
      // above and here errors instead of being truncated (create_new(true)).
      const fd = fs.openSync(abs, 'wx');
      try {
        fs.writeFileSync(fd, pdf);
      } finally {
        fs.closeSync(fd);
      }
    } catch (e) {
      if ((e as NodeJS.ErrnoException)?.code === 'EEXIST') {
        throw new Error(`already exists: ${abs}`);
      }
      throw new Error(`create failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    return { path: abs };
  }

  /** Forward one verb to the wasm session and unwrap its flat `{...,"ok":…}`
   *  reply into a plain result (or throw its error) for `handleLine` to
   *  re-wrap into the standard `{"ok":true,"result":…}` envelope. The wasm
   *  reply must NOT be nested under `result` as-is — it already carries its
   *  own (redundant) inner `"ok"`, which would double-wrap the envelope. */
  private async callWasm(verb: string, args: unknown): Promise<unknown> {
    const raw = await this.host.callWasm(JSON.stringify({ verb, args }));
    const reply = JSON.parse(raw) as Record<string, unknown>;
    if (reply.ok !== true) {
      throw new Error(typeof reply.error === 'string' ? reply.error : 'wasm call failed');
    }
    const rest: Record<string, unknown> = { ...reply };
    delete rest.ok;
    // `undoSteps` is an internal field docxwasm's `doc.replace-range` adds so
    // the host can replay the right number of wasm undos (see `onMutated`). It
    // must NEVER reach the TCP wire — a VS Code tab's reply has to be
    // byte-for-byte a terminal instance's — so strip it here and stash the
    // count for `handleLine` to pass `onMutated`. Reset to 1 on every call so a
    // later non-reporting verb can't inherit a stale count.
    this.lastUndoSteps = typeof rest.undoSteps === 'number' ? rest.undoSteps : 1;
    delete rest.undoSteps;
    // `inverse` is the sibling internal field for the host-orchestrated-inverse
    // bucket (comment add/remove, sheet import-csv/remove). Same rule as
    // `undoSteps`: strip it off the wire (a tab's reply must be byte-for-byte a
    // terminal instance's `{sheet,ref}`/`{removed}`/`{sheet,name,rows,cols}`)
    // and stash it for `handleLine` to pass `onMutated`. Reset to `undefined`
    // first so a later verb without an inverse can't inherit a stale one.
    const inv = rest.inverse;
    this.lastInverse =
      inv && typeof inv === 'object' && typeof (inv as { verb?: unknown }).verb === 'string'
        ? (inv as { verb: string; args: unknown })
        : undefined;
    delete rest.inverse;
    return rest;
  }

  /** `doc.path`/`wb.path`: the host's URI-derived info refreshed with the
   *  live session's block-count/modified fields (`doc.blocks` / `wb.info` —
   *  wasm verbs used internally here, not otherwise exposed to clients).
   *
   *  `host.pathInfo()` owns the *complete* documented reply shape
   *  (`docs/agent-control.md`'s `doc.path`/`wb.path` tables) — `extra` may
   *  only overwrite a key `base` already declares with a fresher value, never
   *  introduce a new one. `doc.blocks`'s own field is named `"total"` (it
   *  doubles as `doc.insert`/`doc.append`'s documented result), not
   *  `"blocks"`, so spreading it in unconditionally would leak an
   *  undocumented `total` key into the wire reply — a VS Code tab's `doc.path`
   *  must be byte-for-byte what a terminal docxy/xlsxy instance sends. */
  private async resolvePathInfo(): Promise<object> {
    const base = (await this.host.pathInfo()) as Record<string, unknown>;
    const infoVerb = this.app === 'docxy' ? 'doc.blocks' : 'wb.info';
    const extra = (await this.callWasm(infoVerb, null)) as Record<string, unknown>;
    const merged: Record<string, unknown> = { ...base };
    for (const key of Object.keys(merged)) {
      if (key in extra) {
        merged[key] = extra[key];
      }
    }
    // `doc.path` additionally carries present-if-set `protection`/`watermark`
    // (docxwasm's `doc.blocks` reports them live, sourced from
    // `Package::protection()`/`watermark()`). The overwrite loop above only
    // refreshes keys `base` already declares, and `host.pathInfo()` doesn't
    // declare these two — so introduce them from `extra` here. A fixed
    // two-key allowlist, NOT a blanket spread of `extra`: `doc.blocks`'s own
    // `total`/`modified` fields must never leak onto the wire, so the merge
    // stays closed to exactly the keys the documented `doc.path` shape allows.
    // Absent (unprotected/unwatermarked) → `doc.blocks` omits them → the
    // `key in extra` guard omits them here too, matching terminal `path_info`.
    for (const key of PATHINFO_MERGE_ALLOWLIST) {
      if (key in extra) {
        merged[key] = extra[key];
      }
    }
    return merged;
  }
}
