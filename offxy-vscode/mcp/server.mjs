// server.mjs — offxy's bundled MCP stdio server: a dependency-free Node ESM
// mirror of `ctlcore::mcp::McpServer` + `docxy/src/mcp.rs` + `xlsxy/src/mcp.rs`
// (see those three files — they are this file's contract). Newline-delimited
// JSON-RPC 2.0 over stdio; `initialize`/`ping`/`tools/list`/`tools/call`;
// tools/call is a thin ctl client: read discovery files under the docxy/xlsxy
// ctl dirs (the SAME dirs terminal docxy/xlsxy and VS Code tabs publish to —
// `ctlcore::config_ctl_dir`), pick the target instance by `resolve_target`'s
// substring semantics, open a fresh TCP connection, send one line, read one
// line back.
//
// This process opens no document itself; it is a bridge to whichever
// docxy/xlsxy the user already has open (a terminal pane or a VS Code tab —
// both advertise into the same ctl dirs, so this server sees both).
//
// No runtime dependencies: only Node built-ins, per the extension's
// zero-dependency bundling policy (ships as-is in the vsix, not esbuilt).

import * as fs from 'node:fs';
import * as net from 'node:net';
import * as os from 'node:os';
import * as path from 'node:path';
import * as readline from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';

const PROTOCOL_VERSION = '2024-11-05';

// Identity: reports as "offxy" (the bundled server for both editors), version
// taken from the extension's own package.json so it never drifts from the
// shipped vsix version.
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const SERVER_NAME = 'offxy';
const SERVER_VERSION = readServerVersion();

function readServerVersion() {
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'package.json'), 'utf8'));
    return typeof pkg.version === 'string' ? pkg.version : '0.0.0';
  } catch {
    return '0.0.0';
  }
}

// ---------------------------------------------------------------------------
// Discovery + ctl client (mirrors ctlcore/src/client.rs — discover/
// discover_live/resolve_target/list_running/Client::call — and
// offxy-vscode/src/ctlserver.ts's private `ctlDir()`)
// ---------------------------------------------------------------------------

/** The directory an app publishes its control discovery files into:
 *  `%APPDATA%\<app>\ctl` on Windows, `$XDG_CONFIG_HOME/<app>/ctl` (falling
 *  back to `~/.config/<app>/ctl`) elsewhere — mirrors `ctlcore::config_ctl_dir`. */
function ctlDir(app) {
  const base =
    process.platform === 'win32'
      ? (process.env.APPDATA ?? path.join(os.homedir(), 'AppData', 'Roaming'))
      : (process.env.XDG_CONFIG_HOME ?? path.join(os.homedir(), '.config'));
  return path.join(base, app, 'ctl');
}

/** Every discovery record in `dir` (any `*.json` that parses as one),
 *  regardless of whether its server is still alive; sorted by instance name —
 *  mirrors `ctlcore::client::discover`. */
function discover(dir) {
  let names;
  try {
    names = fs.readdirSync(dir);
  } catch {
    return [];
  }
  const out = [];
  for (const name of names) {
    if (!name.endsWith('.json')) continue;
    let j;
    try {
      j = JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));
    } catch {
      continue;
    }
    if (
      typeof j.instance === 'string' &&
      // Guard the port to a valid TCP range so a corrupt foreign discovery
      // file (port 0, negative, non-integer, or > 65535) can't make
      // `net.createConnection` throw and fail the whole tool call.
      Number.isInteger(j.port) &&
      j.port > 0 &&
      j.port < 65536 &&
      typeof j.token === 'string'
    ) {
      out.push({ instance: j.instance, port: j.port, token: j.token, pid: typeof j.pid === 'number' ? j.pid : 0 });
    }
  }
  out.sort((a, b) => a.instance.localeCompare(b.instance));
  return out;
}

/** Whether `inst`'s server is currently accepting connections — mirrors
 *  `Instance::is_live`'s 200ms `connect_timeout`. */
function isLive(inst) {
  return new Promise((resolve) => {
    let settled = false;
    const sock = net.createConnection({ host: '127.0.0.1', port: inst.port, timeout: 200 });
    const finish = (ok) => {
      if (settled) return;
      settled = true;
      sock.destroy();
      resolve(ok);
    };
    sock.once('connect', () => finish(true));
    sock.once('timeout', () => finish(false));
    sock.once('error', () => finish(false));
  });
}

/** Discovery records whose server currently accepts a connection — mirrors
 *  `ctlcore::client::discover_live`. */
async function discoverLive(dir) {
  const all = discover(dir);
  const live = await Promise.all(all.map(isLive));
  return all.filter((_inst, i) => live[i]);
}

/** The running instances of `app`, as a JSON tool result:
 *  `{"running":[{instance,port,pid},…]}` — mirrors `ctlcore::client::list_running`. */
async function listRunning(app) {
  const prefix = `${app}-`;
  const running = (await discoverLive(ctlDir(app)))
    .filter((i) => i.instance.startsWith(prefix))
    .map((i) => ({ instance: i.instance, port: i.port, pid: i.pid }));
  return { running };
}

/** Find the single `app` instance to act on: the only one running, or the one
 *  selected by a `target` substring of its instance/pane id — mirrors
 *  `ctlcore::client::resolve_target` (same ambiguity-error wording). */
async function resolveTarget(app, target) {
  const prefix = `${app}-`;
  let live = (await discoverLive(ctlDir(app))).filter((i) => i.instance.startsWith(prefix));
  if (typeof target === 'string') {
    live = live.filter((i) => i.instance.includes(target));
  }
  if (live.length === 0) {
    throw new Error(`no running ${app} found — open a document in a ${app} pane first`);
  }
  if (live.length === 1) {
    return live[0];
  }
  const names = live.map((i) => i.instance).join(', ');
  throw new Error(
    `several ${app} instances are running (${names}); pass "target" with a distinguishing substring (e.g. the pane id)`,
  );
}

/** Like `resolveTarget`, but for tools that can proceed without any instance:
 *  zero live instances with no `target` is `undefined` instead of an error. A
 *  `target` that matches nothing, or an ambiguous candidate set, is still an
 *  error — mirrors `ctlcore::client::resolve_target_for_new`. */
async function resolveTargetForNew(app, target) {
  const prefix = `${app}-`;
  let live = (await discoverLive(ctlDir(app))).filter((i) => i.instance.startsWith(prefix));
  if (typeof target === 'string') {
    live = live.filter((i) => i.instance.includes(target));
    if (live.length === 0) {
      throw new Error(`no running ${app} matches target "${target}"`);
    }
  }
  if (live.length === 0) return undefined;
  if (live.length === 1) return live[0];
  const names = live.map((i) => i.instance).join(', ');
  throw new Error(
    `several ${app} instances are running (${names}); pass "target" with a distinguishing substring (e.g. the pane id)`,
  );
}

const TEMPLATES = {
  docxy: path.join(__dirname, 'templates', 'blank.docx'),
  xlsxy: path.join(__dirname, 'templates', 'blank.xlsx'),
};

/** `docxy_new`/`xlsxy_new`: copy the shipped blank template to an absolutized
 *  path, then open it via the existing open verb — mirrors
 *  `ctlcore::client::new_file` (resolution first: a bad or ambiguous target
 *  creates nothing; no live instance still creates, with `opened:false`). */
async function doNew(app, args) {
  if (typeof args?.path !== 'string') throw new Error('missing path');
  // path.resolve('') falls back to cwd; Rust's std::path::absolute("") errors
  // instead, so match it explicitly before resolving.
  if (args.path === '') throw new Error('bad path: cannot make an empty path absolute');
  const abs = path.resolve(args.path);
  const target = typeof args?.target === 'string' ? args.target : undefined;
  const inst = await resolveTargetForNew(app, target);
  if (fs.existsSync(abs)) throw new Error(`already exists: ${abs}`);
  try {
    // mkdirSync throws EEXIST when a PARENT component is an existing file,
    // not when `abs` itself exists (already ruled out above) — so any error
    // here is a genuine creation failure, never "already exists".
    fs.mkdirSync(path.dirname(abs), { recursive: true });
  } catch (e) {
    throw new Error(`create failed: ${e instanceof Error ? e.message : String(e)}`);
  }
  try {
    // COPYFILE_EXCL: create-exclusive, so a file appearing between the exists
    // check and the copy errors instead of being truncated — mirrors the
    // create_new(true) open in ctlcore::client::new_file.
    fs.copyFileSync(TEMPLATES[app], abs, fs.constants.COPYFILE_EXCL);
  } catch (e) {
    if (e?.code === 'EEXIST') throw new Error(`already exists: ${abs}`);
    throw new Error(`create failed: ${e instanceof Error ? e.message : String(e)}`);
  }
  if (inst === undefined) return JSON.stringify({ path: abs, opened: false });
  try {
    await callInstance(inst, app === 'docxy' ? 'doc.open' : 'wb.open', { path: abs });
  } catch (e) {
    throw new Error(`created ${abs} but open failed: ${e instanceof Error ? e.message : String(e)}`);
  }
  return JSON.stringify({ path: abs, opened: true, instance: inst.instance });
}

/** Send `verb`/`args` to `inst` over a fresh short-lived TCP connection and
 *  return its `result` — or throw a transport failure or the server's own
 *  `{ok:false,error}` message. Mirrors `ctlcore::client::Client::call`
 *  (500ms connect timeout, 10s read timeout, one line out, one line back). */
function callInstance(inst, verb, args) {
  return new Promise((resolve, reject) => {
    let settled = false;
    let connectTimer;
    let readTimer;
    const sock = net.createConnection({ host: '127.0.0.1', port: inst.port });

    const finishOk = (result) => {
      if (settled) return;
      settled = true;
      clearTimeout(connectTimer);
      clearTimeout(readTimer);
      sock.destroy();
      resolve(result);
    };
    const finishErr = (message) => {
      if (settled) return;
      settled = true;
      clearTimeout(connectTimer);
      clearTimeout(readTimer);
      sock.destroy();
      reject(new Error(message));
    };

    // Decode with Node's internal StringDecoder (via setEncoding), not
    // `chunk.toString('utf8')` per chunk: TCP can split a multi-byte UTF-8
    // character (é, €, emoji, CJK) across two `data` events, and decoding each
    // chunk independently would corrupt the straddling bytes to U+FFFD — the
    // same bug fixed in ctlserver.ts (commit a74c19c). setEncoding holds back
    // an incomplete trailing byte sequence until the rest of the character
    // arrives, so `chunk` is always well-formed text here.
    sock.setEncoding('utf8');

    connectTimer = setTimeout(() => finishErr('connect failed: timed out'), 500);
    sock.once('connect', () => {
      clearTimeout(connectTimer);
      readTimer = setTimeout(() => finishErr('read failed: timed out'), 10_000);
      const line = JSON.stringify({ token: inst.token, verb, args }) + '\n';
      sock.write(line);
    });
    sock.once('error', (e) => finishErr(`connect failed: ${e.message}`));

    let buffer = '';
    sock.on('data', (chunk) => {
      buffer += chunk;
      const idx = buffer.indexOf('\n');
      if (idx < 0) return;
      let j;
      try {
        j = JSON.parse(buffer.slice(0, idx).trim());
      } catch (e) {
        finishErr(`bad response: ${e.message}`);
        return;
      }
      if (j.ok === true) {
        finishOk(j.result ?? null);
      } else {
        finishErr(typeof j.error === 'string' ? j.error : 'unknown error');
      }
    });
  });
}

// ---------------------------------------------------------------------------
// Tool definitions (mirrors docxy/src/mcp.rs::tool_defs + xlsxy/src/mcp.rs::tool_defs)
// ---------------------------------------------------------------------------

/** A JSON-schema property: `{"type": ty, "description": desc}` — mirrors
 *  `ctlcore::mcp::prop`. */
function prop(type, description) {
  return { type, description };
}

/** An MCP tool definition with an object input schema — mirrors
 *  `ctlcore::mcp::tool`. */
function tool(name, description, properties, required) {
  return {
    name,
    description,
    inputSchema: { type: 'object', properties, required },
  };
}

/** A JSON-schema property for an array-typed arg — mirrors
 *  `ctlcore::mcp::prop_array`. Compose `items` from `itemTy`/`itemArray`/
 *  `itemObj`. */
function propArray(items, description) {
  return { type: 'array', items, description };
}

/** A bare `{"type": ty}` items schema — mirrors `ctlcore::mcp::item_ty`. */
function itemTy(ty) {
  return { type: ty };
}

/** A bare array items schema wrapping a nested `items` schema — mirrors
 *  `ctlcore::mcp::item_array`. */
function itemArray(items) {
  return { type: 'array', items };
}

/** A bare object items schema — mirrors `ctlcore::mcp::item_obj`. */
function itemObj(properties, required) {
  return { type: 'object', properties, required };
}

const DOCXY_TARGET_DESC =
  'Optional: which docxy to act on (a substring of its instance/pane id) when several are open.';

function docxyToolDefs() {
  const target = () => ['target', prop('string', DOCXY_TARGET_DESC)];
  return [
    tool(
      'docxy_list',
      'List the docxy editors currently running on this machine (instance/pane id, port, pid).',
      {},
      [],
    ),
    tool(
      'docxy_new',
      'Create a new blank .docx at a path and open it in the running docxy (in a VS Code ' +
        'window, a new tab). With no docxy running the file is still created. Refuses to ' +
        'overwrite an existing file.',
      Object.fromEntries([
        ['path', prop('string', 'File path for the new document (created; must not exist).')],
        target(),
      ]),
      ['path'],
    ),
    tool(
      'docxy_status',
      "Report the open document's path, format, modified flag, and block count.",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_outline',
      "Return the document's heading outline: each heading's block index, level, and text.",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_read',
      'Read the live document (including unsaved edits). Returns per-block text + kind; ' +
        'defaults to the whole document, or pass a block range.',
      Object.fromEntries([
        ['start', prop('integer', 'First block index (default 0).')],
        ['end', prop('integer', 'Last block index, inclusive (default: last).')],
        target(),
      ]),
      [],
    ),
    tool(
      'docxy_find',
      'Find all occurrences of a query in the live document; returns match positions and the containing paragraph.',
      Object.fromEntries([
        ['query', prop('string', 'Text to search for.')],
        ['case_sensitive', prop('boolean', 'Match case (default false).')],
        target(),
      ]),
      ['query'],
    ),
    tool(
      'docxy_replace_range',
      'Replace paragraphs [start..=end] with new text (\\n separates paragraphs). Undoable; ' +
        'endpoints must be paragraphs.',
      Object.fromEntries([
        ['start', prop('integer', 'First paragraph block index to replace.')],
        ['end', prop('integer', 'Last paragraph block index, inclusive (default: start).')],
        ['text', prop('string', 'Replacement text; \\n starts a new paragraph.')],
        target(),
      ]),
      ['start', 'text'],
    ),
    tool(
      'docxy_insert',
      'Insert text as new paragraph(s) before the block at `at` (\\n separates paragraphs). Undoable.',
      Object.fromEntries([
        ['at', prop('integer', 'Block index to insert before (== block count to append).')],
        ['text', prop('string', 'Text to insert; \\n starts a new paragraph.')],
        target(),
      ]),
      ['at', 'text'],
    ),
    tool(
      'docxy_append',
      'Append text as new paragraph(s) at the end of the document (\\n separates paragraphs). Undoable.',
      Object.fromEntries([['text', prop('string', 'Text to append; \\n starts a new paragraph.')], target()]),
      ['text'],
    ),
    tool('docxy_save', 'Save the open document to its file.', Object.fromEntries([target()]), []),
    tool(
      'docxy_export',
      'Export the live document (including unsaved edits) as Markdown or plain text.',
      Object.fromEntries([
        ['format', prop('string', 'Output format: "markdown" or "text".')],
        target(),
      ]),
      ['format'],
    ),
    tool(
      'docxy_export_pdf',
      'Render the live document to a PDF at a path. Refuses to overwrite an existing file.',
      Object.fromEntries([
        ['path', prop('string', 'File path for the PDF (created; must not exist).')],
        target(),
      ]),
      ['path'],
    ),
    tool(
      'docxy_comments',
      "List the document's review comments (author, initials, date, text, anchor), in anchor order.",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_notes',
      "List the document's footnotes and endnotes, in file order.",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_header',
      "Read the default section header's block content (empty if the document has none).",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_footer',
      "Read the default section footer's block content (empty if the document has none).",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_metadata',
      "Read the document's core properties (title, author, subject, keywords, comments, " +
        'last saved by, revision, created, modified) — present-if-set.',
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_stats',
      'Word/character/paragraph/block counts over the live document.',
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_replace_all',
      'Replace every occurrence of a query with text across the whole document ' +
        '(case-insensitive unless case_sensitive:true). Undoable.',
      Object.fromEntries([
        ['query', prop('string', 'Text to search for.')],
        ['text', prop('string', 'Replacement text.')],
        ['case_sensitive', prop('boolean', 'Match case (default false).')],
        target(),
      ]),
      ['query', 'text'],
    ),
    tool(
      'docxy_undo',
      'Undo the last edit, if any. Returns {done:false} when there is nothing to undo.',
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'docxy_redo',
      'Redo the last undone edit, if any. Returns {done:false} when there is nothing to redo.',
      Object.fromEntries([target()]),
      [],
    ),
  ];
}

const XLSXY_TARGET_DESC =
  'Optional: which xlsxy to act on (a substring of its instance/pane id) when several are open.';
const SHEET_DESC = 'Optional sheet index or name (default: the active sheet).';

function xlsxyToolDefs() {
  const target = () => ['target', prop('string', XLSXY_TARGET_DESC)];
  const sheet = () => ['sheet', prop('string', SHEET_DESC)];
  return [
    tool(
      'xlsxy_list',
      'List the xlsxy editors currently running on this machine (instance/pane id, port, pid).',
      {},
      [],
    ),
    tool(
      'xlsxy_new',
      'Create a new blank .xlsx at a path and open it in the running xlsxy (in a VS Code ' +
        'window, a new tab). With no xlsxy running the file is still created. Refuses to ' +
        'overwrite an existing file.',
      Object.fromEntries([
        ['path', prop('string', 'File path for the new workbook (created; must not exist).')],
        target(),
      ]),
      ['path'],
    ),
    tool(
      'xlsxy_status',
      "Report the open workbook's path, modified flag, sheet count, and active sheet.",
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'xlsxy_sheets',
      'List every sheet: index, name, and used size (rows/cols).',
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'xlsxy_read',
      'Read non-empty cells of the live workbook (including unsaved edits): value, formula, ' +
        "and display text per cell. Defaults to the active sheet's whole used range, or pass " +
        'an A1-style range.',
      Object.fromEntries([['range', prop('string', 'A1-style range, e.g. "A1:C10".')], sheet(), target()]),
      [],
    ),
    tool(
      'xlsxy_get',
      'Read one cell: value, formula, and display text.',
      Object.fromEntries([['ref', prop('string', 'Cell reference, e.g. "B4".')], sheet(), target()]),
      ['ref'],
    ),
    tool(
      'xlsxy_set',
      "Set a cell. A leading '=' makes a formula (validated + recalculated); otherwise " +
        'number/bool/text is inferred like typing into the grid. Undoable.',
      Object.fromEntries([
        ['ref', prop('string', 'Cell reference, e.g. "B4".')],
        ['text', prop('string', 'What to enter, e.g. "42" or "=SUM(B1:B3)".')],
        sheet(),
        target(),
      ]),
      ['ref', 'text'],
    ),
    tool(
      'xlsxy_clear',
      "Clear a range's values/formulas (styles kept). One undo group.",
      Object.fromEntries([['range', prop('string', 'A1-style range, e.g. "A1:C10".')], sheet(), target()]),
      ['range'],
    ),
    tool(
      'xlsxy_find',
      'Search cell values and formula text (case-insensitive) across all sheets, or one sheet.',
      Object.fromEntries([['query', prop('string', 'Text to search for.')], sheet(), target()]),
      ['query'],
    ),
    tool(
      'xlsxy_recalc',
      'Recalculate the whole workbook (and refresh pivots).',
      Object.fromEntries([target()]),
      [],
    ),
    tool('xlsxy_save', 'Save the open workbook to its file.', Object.fromEntries([target()]), []),
    tool(
      'xlsxy_comments',
      'List every cell comment (threads flattened in reply order): sheet, cell ref, author, text.',
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'xlsxy_comment_add',
      'Add a threaded comment to a cell (or a reply, if the cell already has a thread).',
      Object.fromEntries([
        ['ref', prop('string', 'Cell reference, e.g. "B4".')],
        ['text', prop('string', 'Comment text.')],
        ['author', prop('string', 'Comment author (defaults to the editing identity).')],
        sheet(),
        target(),
      ]),
      ['ref', 'text'],
    ),
    tool(
      'xlsxy_comment_remove',
      'Remove the comment (threaded or legacy note) on a cell, if any.',
      Object.fromEntries([['ref', prop('string', 'Cell reference, e.g. "B4".')], sheet(), target()]),
      ['ref'],
    ),
    tool(
      'xlsxy_range_set',
      'Write a rectangular block of cells starting at a top-left ref, atomically: every ' +
        'formula in the batch is validated before anything is applied. One undo group.',
      Object.fromEntries([
        ['start', prop('string', 'Top-left cell reference, e.g. "B4".')],
        [
          'rows',
          propArray(
            itemArray(itemTy('string')),
            "Rows of cell text, each row an array of strings entered like xlsxy_set's " +
              'text (empty string clears the cell).',
          ),
        ],
        sheet(),
        target(),
      ]),
      ['start', 'rows'],
    ),
    tool(
      'xlsxy_export_csv',
      "Export a sheet's cells as display-formatted, RFC-4180 CSV.",
      Object.fromEntries([sheet(), target()]),
      [],
    ),
    tool(
      'xlsxy_import_csv',
      'Import CSV text as a brand-new sheet (never overwrites an existing one; name ' +
        'collisions are deduplicated).',
      Object.fromEntries([
        ['text', prop('string', 'CSV text to import.')],
        ['name', prop('string', 'Requested sheet name (default: "Sheet", deduplicated).')],
        target(),
      ]),
      ['text'],
    ),
    tool(
      'xlsxy_pivot',
      'Compute an ad-hoc pivot table over a range (first row = header names); read-only, no ' +
        'workbook mutation.',
      Object.fromEntries([
        ['range', prop('string', 'A1-style range, e.g. "A1:D100", first row = headers.')],
        ['rows', propArray(itemTy('string'), 'Header names to group by, as pivot rows.')],
        ['cols', propArray(itemTy('string'), 'Header names to group by, as pivot columns.')],
        [
          'values',
          propArray(
            itemObj(
              Object.fromEntries([
                ['col', prop('string', 'Header name of the column to aggregate.')],
                [
                  'agg',
                  prop(
                    'string',
                    'Aggregation: sum, count, countNums, average, max, min, ' +
                      'product, stdDev, stdDevP, var, or varP.',
                  ),
                ],
              ]),
              ['col', 'agg'],
            ),
            'Measures to compute, each a {col, agg} pair.',
          ),
        ],
        sheet(),
        target(),
      ]),
      ['range', 'rows', 'values'],
    ),
    tool(
      'xlsxy_replace_all',
      "Literal find/replace across every cell's input text, on every sheet. One undo group.",
      Object.fromEntries([
        ['query', prop('string', 'Text to search for.')],
        ['text', prop('string', 'Replacement text.')],
        target(),
      ]),
      ['query', 'text'],
    ),
    tool(
      'xlsxy_sheet_add',
      'Add a new sheet (deduplicated name on collision — never errors on a taken name).',
      Object.fromEntries([
        ['name', prop('string', 'Requested sheet name (default: "Sheet", deduplicated).')],
        target(),
      ]),
      [],
    ),
    tool(
      'xlsxy_sheet_remove',
      'Remove a sheet (errors on the last one — a workbook must keep at least one).',
      Object.fromEntries([
        ['sheet', prop('string', 'Sheet index or name to remove.')],
        target(),
      ]),
      ['sheet'],
    ),
    tool(
      'xlsxy_sheet_rename',
      'Rename a sheet and rewrite every formula/defined-name reference to it.',
      Object.fromEntries([
        ['sheet', prop('string', 'Sheet index or name to rename.')],
        ['name', prop('string', 'New sheet name.')],
        target(),
      ]),
      ['sheet', 'name'],
    ),
    tool(
      'xlsxy_row_insert',
      'Insert rows at a 0-based row index.',
      Object.fromEntries([
        ['at', prop('integer', '0-based row index to insert at.')],
        ['count', prop('integer', 'Number of rows to insert (default 1).')],
        sheet(),
        target(),
      ]),
      ['at'],
    ),
    tool(
      'xlsxy_row_delete',
      'Delete rows at a 0-based row index.',
      Object.fromEntries([
        ['at', prop('integer', '0-based row index to delete from.')],
        ['count', prop('integer', 'Number of rows to delete (default 1).')],
        sheet(),
        target(),
      ]),
      ['at'],
    ),
    tool(
      'xlsxy_col_insert',
      'Insert columns at a 0-based column index.',
      Object.fromEntries([
        ['at', prop('integer', '0-based column index to insert at.')],
        ['count', prop('integer', 'Number of columns to insert (default 1).')],
        sheet(),
        target(),
      ]),
      ['at'],
    ),
    tool(
      'xlsxy_col_delete',
      'Delete columns at a 0-based column index.',
      Object.fromEntries([
        ['at', prop('integer', '0-based column index to delete from.')],
        ['count', prop('integer', 'Number of columns to delete (default 1).')],
        sheet(),
        target(),
      ]),
      ['at'],
    ),
    tool(
      'xlsxy_eval',
      'Side-effect-free formula preview: evaluate a formula against the live workbook at a ' +
        'cell without writing anywhere.',
      Object.fromEntries([
        ['formula', prop('string', 'Formula to evaluate, e.g. "SUM(B1:B3)" (leading \'=\' optional).')],
        ['ref', prop('string', 'Cell to evaluate at, e.g. "B4" (default A1).')],
        sheet(),
        target(),
      ]),
      ['formula'],
    ),
    tool(
      'xlsxy_stats',
      'Summary statistics (sum, count, countNums, average, min, max) over a range.',
      Object.fromEntries([
        ['range', prop('string', 'A1-style range, e.g. "A1:C10".')],
        sheet(),
        target(),
      ]),
      ['range'],
    ),
    tool(
      'xlsxy_charts',
      'List every chart in the workbook: kind, title, categories, and series.',
      Object.fromEntries([target()]),
      [],
    ),
    tool(
      'xlsxy_pivots',
      'List every persistent pivot table: sheet, row/column fields, and value fields.',
      Object.fromEntries([target()]),
      [],
    ),
  ];
}

const TOOLS = [...docxyToolDefs(), ...xlsxyToolDefs()];

// ---------------------------------------------------------------------------
// Tool execution (mirrors docxy/src/mcp.rs::do_tool + xlsxy/src/mcp.rs::do_tool)
// ---------------------------------------------------------------------------

const DOCXY_VERBS = {
  docxy_status: 'doc.path',
  docxy_outline: 'doc.outline',
  docxy_read: 'doc.read',
  docxy_find: 'doc.find',
  docxy_replace_range: 'doc.replace-range',
  docxy_insert: 'doc.insert',
  docxy_append: 'doc.append',
  docxy_save: 'doc.save',
  docxy_export: 'doc.export',
  docxy_export_pdf: 'doc.export-pdf',
  docxy_comments: 'doc.comments',
  docxy_notes: 'doc.notes',
  docxy_header: 'doc.header',
  docxy_footer: 'doc.footer',
  docxy_metadata: 'doc.metadata',
  docxy_stats: 'doc.stats',
  docxy_replace_all: 'doc.replace-all',
  docxy_undo: 'doc.undo',
  docxy_redo: 'doc.redo',
};

const XLSXY_VERBS = {
  xlsxy_status: 'wb.path',
  xlsxy_sheets: 'sheet.list',
  xlsxy_read: 'sheet.read',
  xlsxy_get: 'cell.get',
  xlsxy_set: 'cell.set',
  xlsxy_clear: 'range.clear',
  xlsxy_find: 'find',
  xlsxy_recalc: 'wb.recalc',
  xlsxy_save: 'wb.save',
  xlsxy_comments: 'comment.list',
  xlsxy_comment_add: 'comment.add',
  xlsxy_comment_remove: 'comment.remove',
  xlsxy_range_set: 'range.set',
  xlsxy_export_csv: 'wb.export-csv',
  xlsxy_import_csv: 'sheet.import-csv',
  xlsxy_pivot: 'sheet.pivot',
  xlsxy_replace_all: 'wb.replace-all',
  xlsxy_sheet_add: 'sheet.add',
  xlsxy_sheet_remove: 'sheet.remove',
  xlsxy_sheet_rename: 'sheet.rename',
  xlsxy_row_insert: 'row.insert',
  xlsxy_row_delete: 'row.delete',
  xlsxy_col_insert: 'col.insert',
  xlsxy_col_delete: 'col.delete',
  xlsxy_eval: 'formula.eval',
  xlsxy_stats: 'sheet.stats',
  xlsxy_charts: 'chart.list',
  xlsxy_pivots: 'pivot.list',
};

/** Execute a tool by forwarding to the control surface. Returns the result
 *  text (JSON) or throws an `Error` whose `.message` becomes the tool's
 *  `isError` text — mirrors docxy's/xlsxy's `do_tool`. */
async function doTool(name, args) {
  if (name === 'docxy_list') {
    return JSON.stringify(await listRunning('docxy'));
  }
  if (name === 'xlsxy_list') {
    return JSON.stringify(await listRunning('xlsxy'));
  }
  if (name === 'docxy_new') return doNew('docxy', args);
  if (name === 'xlsxy_new') return doNew('xlsxy', args);
  let app;
  let verb;
  if (Object.prototype.hasOwnProperty.call(DOCXY_VERBS, name)) {
    app = 'docxy';
    verb = DOCXY_VERBS[name];
  } else if (Object.prototype.hasOwnProperty.call(XLSXY_VERBS, name)) {
    app = 'xlsxy';
    verb = XLSXY_VERBS[name];
  } else {
    throw new Error(`unknown tool: ${name}`);
  }
  const target = typeof args?.target === 'string' ? args.target : undefined;
  const inst = await resolveTarget(app, target);
  // Control verbs ignore unknown keys, so forwarding `args` verbatim
  // (including `target`) is harmless — mirrors the Rust adapters' comment.
  const result = await callInstance(inst, verb, args ?? {});
  return JSON.stringify(result);
}

// ---------------------------------------------------------------------------
// MCP JSON-RPC scaffolding (mirrors ctlcore/src/mcp.rs::McpServer)
// ---------------------------------------------------------------------------

function ok(id, result) {
  return { jsonrpc: '2.0', id, result };
}

function err(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } };
}

function toolResultEnvelope(text, isError) {
  return { content: [{ type: 'text', text }], isError };
}

function initializeResult() {
  return {
    protocolVersion: PROTOCOL_VERSION,
    capabilities: { tools: {} },
    serverInfo: { name: SERVER_NAME, version: SERVER_VERSION },
  };
}

async function handleToolCall(id, params) {
  if (params === undefined || params === null || typeof params !== 'object') {
    return err(id, -32602, 'missing params');
  }
  const name = params.name;
  if (typeof name !== 'string') {
    return err(id, -32602, 'missing tool name');
  }
  const args =
    params.arguments !== undefined && params.arguments !== null && typeof params.arguments === 'object'
      ? params.arguments
      : {};
  try {
    const text = await doTool(name, args);
    return ok(id, toolResultEnvelope(text, false));
  } catch (e) {
    return ok(id, toolResultEnvelope(e instanceof Error ? e.message : String(e), true));
  }
}

/** Route one JSON-RPC message. Resolves `response` for requests, `undefined`
 *  for notifications (and messages without a method) — mirrors
 *  `McpServer::handle`. */
async function handle(msg) {
  const method = typeof msg?.method === 'string' ? msg.method : undefined;
  if (method === undefined) return undefined;
  const id = msg.id !== undefined ? msg.id : null;

  if (method === 'initialize') {
    return ok(id, initializeResult());
  }
  if (method === 'ping') {
    return ok(id, {});
  }
  if (method === 'tools/list') {
    return ok(id, { tools: TOOLS });
  }
  if (method === 'tools/call') {
    return handleToolCall(id, msg.params);
  }
  if (method.startsWith('notifications/')) {
    return undefined;
  }
  return err(id, -32601, `method not found: ${method}`);
}

// ---------------------------------------------------------------------------
// stdio transport: one message per line, processed strictly in order (a
// promise-chain queue, same reasoning as `CtlServer`'s in `ctlserver.ts`) so
// concurrent tool calls can't race their replies out of order.
// ---------------------------------------------------------------------------

async function processLine(line) {
  const trimmed = line.trim();
  if (trimmed === '') return;
  let msg;
  try {
    msg = JSON.parse(trimmed);
  } catch {
    return; // ignore anything that isn't a JSON message
  }
  const resp = await handle(msg);
  if (resp !== undefined) {
    process.stdout.write(JSON.stringify(resp) + '\n');
  }
}

function main() {
  const rl = readline.createInterface({ input: process.stdin, terminal: false });
  let queue = Promise.resolve();
  rl.on('line', (line) => {
    queue = queue.then(() => processLine(line)).catch((e) => {
      process.stderr.write(`offxy mcp server: ${e instanceof Error ? e.stack : String(e)}\n`);
    });
  });
}

// Only start the stdio server loop when this file is run directly (`node
// mcp/server.mjs`), not when a test/parity harness `import`s it to reach
// TOOLS/DOCXY_VERBS/XLSXY_VERBS — an imported module has no business reading
// stdin. `process.argv[1]` is the entry script Node was invoked with; compare
// as a file URL (not a raw path string) so drive-letter casing/slash-style
// differences on Windows can't make this false-negative.
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}

export { TOOLS, DOCXY_VERBS, XLSXY_VERBS };
