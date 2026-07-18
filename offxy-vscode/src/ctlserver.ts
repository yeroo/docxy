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
   *  dot + undo lockstep). Not called for read-only or failed verbs.
   *
   *  `undoSteps` is how many native wasm undo checkpoints the edit pushed — the
   *  provider must replay exactly this many wasm undos per one VS Code undo, or
   *  the two stacks desync. Only `doc.replace-range` ever reports 2 (a
   *  delete-then-insert; 1 when the range was a single empty paragraph); every
   *  other mutating verb checkpoints once. Defaults to 1 when absent. */
  onMutated(verbLabel: string, undoSteps?: number): void;
}

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
        this.host.onMutated(verb, this.lastUndoSteps);
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
    if (this.wasmVerbs.has(verb)) {
      return this.callWasm(verb, args);
    }
    throw new Error(`unknown verb '${verb}'`);
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
    return merged;
  }
}
