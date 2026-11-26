// The PDF viewer's side of its worker (#2259), the suite's `Worker` rule in a
// webview: one request at a time, each with a timeout. Past it, or when the
// worker traps or dies, the worker is ended, that request answers why
// (`{error, lost: true}`), and a new worker opens the same bytes for the next
// one. A worker that cannot even open the file is not started again.
//
// No DOM here: `makeWorker` and the timers are given, so pdf.test.mjs drives
// it with fake workers.
(function (root) {
  'use strict';

  /** The longest one request may take: the suite's (convert_child::TIMEOUT). */
  const TIMEOUT_MS = 20000;
  /** What a request past its timeout answers. */
  const TOO_LONG = 'it took too long to draw';
  /** What a request whose worker trapped or died answers. */
  const STOPPED = 'the PDF reader stopped on it';

  class PdfHost {
    /**
     * @param {object} o
     * @param {() => Worker} o.makeWorker a fresh worker
     * @param {WebAssembly.Module} o.module the compiled engine
     * @param {Uint8Array} o.bytes the file
     * @param {(info: object) => void} o.onInfo the first worker's `info`
     */
    constructor(o) {
      this.makeWorker = o.makeWorker;
      this.module = o.module;
      this.bytes = o.bytes;
      this.onInfo = o.onInfo;
      this.timeoutMs = o.timeoutMs ?? TIMEOUT_MS;
      this.setTimer = o.setTimer ?? ((f, ms) => setTimeout(f, ms));
      this.clearTimer = o.clearTimer ?? ((t) => clearTimeout(t));
      this.queue = [];
      this.current = null;
      this.nextId = 1;
      this.worker = null;
      this.ready = false;
      this.opened = false;
      this.dead = null;
      this.timer = null;
      /** Workers started, for the tests. */
      this.starts = 0;
      this.start();
    }

    start() {
      this.starts += 1;
      this.ready = false;
      const w = this.makeWorker();
      this.worker = w;
      w.onmessage = (event) => {
        if (this.worker === w) this.message(event.data);
      };
      w.onerror = (event) => {
        if (event && event.preventDefault) event.preventDefault();
        if (this.worker === w) this.lost(STOPPED);
      };
      // Opening is a request too: a file that spins in it is ended.
      this.arm();
      // The bytes are copied, never transferred: the next worker needs them.
      w.postMessage({ type: 'init', module: this.module, bytes: this.bytes });
    }

    arm() {
      this.disarm();
      this.timer = this.setTimer(() => this.lost(TOO_LONG), this.timeoutMs);
    }

    disarm() {
      if (this.timer !== null) this.clearTimer(this.timer);
      this.timer = null;
    }

    /** Ask the worker `req` (`{type:'render', page, scale}` or `{type:'text', page}`). */
    request(req) {
      return new Promise((resolve) => {
        if (this.dead) {
          resolve({ error: this.dead, lost: true });
          return;
        }
        this.queue.push({ req, resolve });
        this.pump();
      });
    }

    pump() {
      if (!this.ready || this.current || this.dead) return;
      const next = this.queue.shift();
      if (!next) return;
      this.current = { id: this.nextId++, ...next };
      this.arm();
      this.worker.postMessage({ ...next.req, id: this.current.id });
    }

    message(msg) {
      if (msg.type === 'info') {
        this.disarm();
        this.ready = true;
        if (!this.opened) {
          this.opened = true;
          this.onInfo(msg.info);
        }
        this.pump();
        return;
      }
      if (msg.type === 'trap') {
        this.lost(msg.error ? `${STOPPED} (${msg.error})` : STOPPED);
        return;
      }
      if (msg.type === 'done' && this.current && msg.id === this.current.id) {
        this.disarm();
        const { resolve } = this.current;
        this.current = null;
        resolve(msg);
        this.pump();
      }
    }

    /** The worker is gone (`why`): answer what it held and start another. */
    lost(why) {
      this.disarm();
      if (this.worker) this.worker.terminate();
      this.worker = null;
      const held = this.current;
      this.current = null;
      if (held) {
        held.resolve({ error: why, lost: true });
      }
      if (!this.ready && !held) {
        // It died opening the file: no worker will do better.
        this.dead = why;
        if (!this.opened) {
          this.opened = true;
          this.onInfo({ ok: false, error: why, locked: false });
        }
        for (const q of this.queue.splice(0)) q.resolve({ error: why, lost: true });
        return;
      }
      this.start();
    }

    dispose() {
      this.disarm();
      if (this.worker) this.worker.terminate();
      this.worker = null;
      this.dead = 'the view was closed';
      const held = this.current;
      this.current = null;
      if (held) held.resolve({ error: this.dead, lost: true });
      for (const q of this.queue.splice(0)) q.resolve({ error: this.dead, lost: true });
    }
  }

  const api = { PdfHost, TIMEOUT_MS, TOO_LONG, STOPPED };
  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  } else {
    root.PdfHostApi = api;
  }
})(typeof self !== 'undefined' ? self : this);
