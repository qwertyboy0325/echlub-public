// The browser demo's shell: load the wasm app and the song, start the app
// on the canvas, and keep the engine running. The engine lives in the
// app's wasm instance on this (the main) thread; an AudioWorklet only plays
// the blocks this loop renders and posts to it (no SharedArrayBuffer).
// Before sound is started the loop runs the engine on the page's clock and
// drops what it renders, so the transport still moves.
import init, { WebHandle } from "./echlub_web_demo.js";

const $ = (id) => document.getElementById(id);
const params = new URLSearchParams(location.search);
const RATE = 48000;
// The song: a folder under songs/ (manifest.json and the files it names),
// `?song=<name>` or the first one songs/index.json lists.
let SONG = "";
// Frames rendered ahead of what the player has played: what the queue
// rides out when the main thread stalls (an egui frame, a GC).
// A phone or tablet (a touch screen as the main pointer) gets a full second:
// its slower CPU stalls longer, and nobody plays it live from a keyboard there.
const TOUCH = typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;
const AHEAD = Math.round(Number(params.get("ahead") || (TOUCH ? 1 : 0.25)) * RATE);
// Frames of playing blocks the player hashes (the check against the native reference).
const HASH_FRAMES = Math.round(Number(params.get("hash_s") || 10) * RATE);

const report = {};
const setReport = (k, v) => { report[k] = v; $("report").textContent = JSON.stringify(report); };
function fail(msg) {
  document.body.dataset.state = "error";
  $("load-status").textContent = `Failed: ${msg}`;
  setReport("error", String(msg));
}
let toastTimer = 0;
// `link` ({url, name}): a saved file's link, offered in the toast for a while
// (a browser may refuse a download that no click of the visitor started).
// `ms`: how long a plain toast stays (default 4 s).
function toast(text, link, ms = 4000) {
  const t = $("toast");
  t.textContent = text;
  t.dataset.link = link ? "1" : "0";
  if (link) {
    const a = document.createElement("a");
    a.href = link.url; a.download = link.name; a.textContent = "Save it again";
    t.append(" ", a);
  }
  t.dataset.show = "1";
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.dataset.show = "0"; }, link ? 30000 : ms);
}

const bytes = async (url, optional = false) => {
  const r = await fetch(url);
  if (!r.ok) { if (optional) return new Uint8Array(0); throw new Error(`${url}: HTTP ${r.status}`); }
  return new Uint8Array(await r.arrayBuffer());
};

const pump = {
  handle: null, block: 128, sent: 0, consumed: 0, mode: "clock", t0: 0,
  ctx: null, node: null, player: { underruns: 0, missing: 0, queued: 0, hash: null, hashFrames: 0, nonzero: 0 },
  rendered: 0, renderMs: 0, maxTickMs: 0,
  // Loudest sample of the blocks rendered since the page (or a check) last reset it, heard or not: what an audition test reads.
  peak: 0,
};

function tick() {
  const p = pump;
  if (p.mode === "clock") p.consumed = Math.floor((performance.now() - p.t0) * RATE / 1000);
  let behind = AHEAD - (p.sent - p.consumed);
  // Far behind (a hidden tab, a long stall): skip, do not try to catch up.
  if (behind > RATE) { p.sent = p.consumed + AHEAD - RATE / 4; behind = RATE / 4; }
  const blocks = Math.ceil(behind / p.block);
  if (blocks <= 0) return;
  const t = performance.now();
  const samples = p.handle.pump(blocks);
  const flags = p.handle.pump_flags();
  const ms = performance.now() - t;
  p.renderMs += ms; p.maxTickMs = Math.max(p.maxTickMs, ms);
  let pk = p.peak;
  for (let i = 0; i < samples.length; i++) { const v = Math.abs(samples[i]); if (v > pk) pk = v; }
  p.peak = pk;
  p.rendered += blocks * p.block;
  p.sent += blocks * p.block;
  if (p.mode === "audio") p.node.port.postMessage({ samples, flags, block: p.block }, [samples.buffer]);
}

// Copies of unsaved work, kept in the browser's own storage (IndexedDB: a
// project's file is megabytes of text, past what localStorage keeps, and
// its calls are asynchronous). The app queues a copy on its autosave rhythm
// and when a document is saved or replaced it asks for the copy to go; this
// puts them in a store of one database, one record per visit, and gives the
// earlier visits' records back at start-up for the recovery question. Where
// the storage is not there (a private window, blocked site data) or full,
// the page runs as before and says once that work will not be kept.
const DB = "echlub-web-demo", STORE = "copies";
const storage = {
  db: null, ok: false, told: false, toasts: 0, error: "", puts: 0, deletes: 0, offered: 0, lastBytes: 0, busy: false,
  key: `visit-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`,
  say(why) {
    this.ok = false; this.error = String(why);
    $("keep").textContent = "Unsaved work is NOT kept across reloads."; $("keep").style.color = "var(--bad)";
    if (this.told) return;
    this.told = true; this.toasts++;
    toast(`This browser is not letting the page keep your work (${why}), so it will be lost if the page is reloaded or closed. Use File → Save to keep the project.`, null, 14000);
  },
  req(r) { return new Promise((res, rej) => { r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error || new Error("storage request failed")); }); },
  // Opens the database and returns the earlier visits' copies as [[key, text]].
  async open() {
    try {
      if (typeof indexedDB === "undefined" || !indexedDB) throw new Error("no IndexedDB");
      const open = indexedDB.open(DB, 1);
      open.onupgradeneeded = () => { open.result.createObjectStore(STORE); };
      this.db = await new Promise((res, rej) => {
        open.onsuccess = () => res(open.result);
        open.onerror = () => rej(open.error || new Error("cannot open"));
        open.onblocked = () => rej(new Error("blocked"));
      });
      const tx = this.db.transaction(STORE, "readonly");
      const os = tx.objectStore(STORE);
      const [keys, vals] = await Promise.all([this.req(os.getAllKeys()), this.req(os.getAll())]);
      this.ok = true;
      const dec = new TextDecoder();
      const copies = [];
      keys.forEach((k, i) => { try { copies.push([String(k), dec.decode(vals[i])]); } catch (_) { /* unreadable: the app drops it */ } });
      this.offered = copies.length;
      return copies;
    } catch (e) {
      this.say(e && e.message ? e.message : e);
      return [];
    } finally {
      setReport("storage", this.state());
    }
  },
  state() { return { ok: this.ok, error: this.error, toasts: this.toasts, key: this.key, puts: this.puts, deletes: this.deletes, offered: this.offered, lastBytes: this.lastBytes }; },
  async apply(kind, key, bytes) {
    if (!this.db || !this.ok) return;
    try {
      const tx = this.db.transaction(STORE, "readwrite");
      const done = new Promise((res, rej) => { tx.oncomplete = res; tx.onabort = () => rej(tx.error || new Error("aborted")); tx.onerror = () => rej(tx.error || new Error("failed")); });
      if (kind === "put") tx.objectStore(STORE).put(bytes, key); else tx.objectStore(STORE).delete(key);
      await done;
      if (kind === "put") { this.puts++; this.lastBytes = bytes.length; } else this.deletes++;
    } catch (e) {
      const full = e && (e.name === "QuotaExceededError" || /quota/i.test(String(e.message)));
      this.say(full ? "its storage is full" : (e && e.message ? e.message : e));
    }
    setReport("storage", this.state());
  },
};
// The session's queue of storage operations, in order, a few a turn.
async function drainStore() {
  if (storage.busy || !pump.handle) return;
  storage.busy = true;
  try {
    for (let n = 0; n < 8; n++) {
      const op = pump.handle.store_op();
      if (!op) return;
      const [kind, key] = op.split("\t");
      // A copy of the view: the module's memory moves when it grows.
      const bytes = kind === "put" ? pump.handle.store_bytes().slice() : null;
      pump.handle.store_done();
      await storage.apply(kind, key, bytes);
    }
  } finally { storage.busy = false; }
}

// A finished export or saved file (the app's File menu: WAV, FLAC, a
// project, a MIDI file) is a file in the module's memory: make a Blob of
// it straight from there, free the module's copy, and let the browser save
// the Blob. The object URL goes later, when the browser has surely started
// the save.
const MIME = { flac: "audio/flac", wav: "audio/wav", json: "application/json", mid: "audio/midi" };
const sizeText = (n) => (n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
const saved = { count: 0, last: "", bytes: 0 };
function saveDownloads() {
  for (let n = 0; n < 8; n++) {
    const name = pump.handle.download_name();
    if (!name) return;
    const view = pump.handle.download_bytes();
    const blob = new Blob([view], { type: MIME[name.split(".").pop().toLowerCase()] || "application/octet-stream" });
    pump.handle.download_done();
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url; a.download = name; a.hidden = true;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 120000);
    saved.count++; saved.last = name; saved.bytes = blob.size;
    setReport("downloads", { ...saved });
    toast(`Saved ${name} (${sizeText(blob.size)}) to your downloads.`, { url, name });
  }
}

// A file the app asks for (File → Open, Import MIDI): the app files a
// request, this shows the browser's own file chooser once for it and hands
// the chosen file's bytes to the app (or says the visitor backed out). A
// chooser opens only for a recent click or tap on the page; if that has
// gone by (the app answered slowly) a button in the shell asks for a fresh one.
const picking = { id: 0, last: null };
function checkPick() {
  const raw = pump.handle && pump.handle.pick_request();
  if (!raw) return;
  const req = JSON.parse(raw);
  if (req.id === picking.id) return;
  picking.id = req.id;
  const input = document.createElement("input");
  input.type = "file"; input.accept = req.accept; input.hidden = true;
  const bar = $("pickbar");
  const close = () => { bar.hidden = true; input.remove(); };
  input.addEventListener("change", async () => {
    const f = input.files && input.files[0];
    close();
    if (!f) { pump.handle.pick_cancel(req.id); return; }
    try {
      const buf = new Uint8Array(await f.arrayBuffer());
      const taken = pump.handle.pick_deliver(req.id, f.name, buf);
      picking.last = { name: f.name, bytes: buf.length, taken };
    } catch (e) {
      pump.handle.pick_cancel(req.id);
      toast(`Could not read ${f.name}: ${e && e.message ? e.message : e}`);
    }
    setReport("picked", picking.last);
  });
  input.addEventListener("cancel", () => { close(); pump.handle.pick_cancel(req.id); });
  document.body.appendChild(input);
  const fresh = !navigator.userActivation || navigator.userActivation.isActive;
  if (fresh) { input.click(); return; }
  $("pick-label").textContent = `Choose a file: ${req.label}`;
  bar.hidden = false;
  $("pick-go").onclick = () => { bar.hidden = true; input.click(); };
  $("pick-no").onclick = () => { close(); pump.handle.pick_cancel(req.id); };
}

const slow = { consumed: 0, missing: 0, told: false };
function showAudio() {
  const p = pump, q = p.player;
  if (p.mode === "clock") return;
  const speed = p.renderMs > 0 ? (p.rendered / RATE) / (p.renderMs / 1000) : 0;
  const out = p.ctx.outputLatency ? `, output latency reported ${(p.ctx.outputLatency * 1000).toFixed(0)} ms` : "";
  $("audio").textContent = `Sound on. Queue ${(q.queued / RATE * 1000).toFixed(0)} ms (target ${(AHEAD / RATE * 1000).toFixed(0)} ms), ` +
    `underruns ${q.underruns}; engine ${speed.toFixed(1)}× real time${out} (measured on this machine)`;
  // This device cannot render the song as fast as it plays: the sound cuts
  // out. Say so once (over a few seconds of playing, more than 5% missing).
  const w = slow;
  if (q.consumed < w.consumed) { w.consumed = 0; w.missing = 0; } // sound restarted: the player's counts reset
  if (q.consumed > w.consumed + RATE * 4) {
    const missed = q.missing - w.missing, played = q.consumed - w.consumed;
    if (!w.told && missed > 0.05 * (missed + played)) {
      w.told = true;
      setReport("tooSlow", true);
      toast(`This device cannot keep up with this song (${Math.round(100 * missed / (missed + played))}% of the sound is missing). ` +
        "Try the Built-in trio song from the song menu, or open the demo on a computer.");
    }
    w.consumed = q.consumed; w.missing = q.missing;
  }
  const ref = pump.reference;
  if (q.hashDone && ref) {
    const same = q.hash === ref.hash && q.hashFrames === ref.frames;
    setReport("hashCheck", { page: q.hash, native: ref.hash, frames: q.hashFrames, same });
    $("audio").textContent += same ? `. First ${(q.hashFrames / RATE).toFixed(0)} s played: bit-identical to the native reference.`
      : `. First ${(q.hashFrames / RATE).toFixed(0)} s played: differs from the native reference (${q.hash} vs ${ref.hash}).`;
  }
  setReport("audio", { ...q, sent: p.sent, consumed: p.consumed, speed, maxTickMs: p.maxTickMs, baseLatency: p.ctx.baseLatency, outputLatency: p.ctx.outputLatency, state: p.ctx.state });
}

async function startSound() {
  const p = pump;
  if (p.mode === "audio") {
    await p.ctx.suspend();
    p.node.disconnect();
    p.mode = "clock"; p.t0 = performance.now() - p.consumed * 1000 / RATE;
    $("sound").textContent = "Start sound"; $("sound").setAttribute("aria-pressed", "false");
    $("audio").textContent = "Sound off: the engine runs on the page's clock, silently.";
    return;
  }
  // Phones (iOS Safari above all) let sound start only inside the tap
  // itself: resume the context before the first await, while the tap still
  // counts, and ask for the playback audio session, which the ring/silent
  // switch does not mute (Safari 17+; elsewhere the property is absent).
  try { if (navigator.audioSession) navigator.audioSession.type = "playback"; } catch (_) { /* not offered */ }
  if (!p.ctx) {
    p.ctx = new AudioContext({ sampleRate: RATE, latencyHint: "interactive" });
    const resumed = p.ctx.resume();
    await p.ctx.audioWorklet.addModule("player.js");
    await resumed;
    p.node = new AudioWorkletNode(p.ctx, "echlub-player", { numberOfInputs: 0, outputChannelCount: [2], processorOptions: { hashFrames: HASH_FRAMES } });
    p.node.port.onmessage = (e) => { Object.assign(p.player, e.data); p.consumed = e.data.consumed; };
  }
  p.node.port.postMessage({ reset: true });
  p.node.connect(p.ctx.destination);
  await p.ctx.resume();
  if (p.ctx.state !== "running") toast("The browser kept the sound off. Tap Start sound again; on an iPhone, also check the silent switch.");
  // The queue starts empty: what the clock mode rendered was never heard.
  p.sent = 0; p.consumed = 0; p.mode = "audio";
  $("sound").textContent = "Stop sound"; $("sound").setAttribute("aria-pressed", "true");
}

async function main() {
  await init({ module_or_path: "echlub_web_demo_bg.wasm" });
  const index = await (await fetch("songs/index.json")).json();
  const pick = (params.get("song") || index.songs[0]).replace(/[^A-Za-z0-9_-]/g, "");
  if (!index.songs.includes(pick)) throw new Error(`no song "${pick}" (songs: ${index.songs.join(", ")})`);
  SONG = `songs/${pick}`;
  setReport("song", pick);
  const manifest = await (await fetch(`${SONG}/manifest.json`)).text();
  const m = JSON.parse(manifest);
  pump.reference = m.reference || null;
  $("load-title").textContent = m.title || "EchLub DAW";
  $("song").textContent = m.title ? `Song: ${m.title}` : "";
  if (m.credits) { $("credits").href = `${SONG}/${m.credits}`; $("credits").hidden = false; }
  for (const n of index.songs) {
    const o = document.createElement("option");
    o.value = n; o.textContent = (index.titles && index.titles[n]) || n; o.selected = n === pick;
    $("pick").appendChild(o);
  }
  $("pick").hidden = index.songs.length < 2;
  $("pick").addEventListener("change", () => { params.set("song", $("pick").value); location.search = params.toString(); });
  const names = WebHandle.song_files(manifest);
  const files = await Promise.all(names.map((n) => bytes(`${SONG}/${n}`)));
  const font = await bytes(params.get("font") || "fonts/NotoSansTC.ttf", true);
  setReport("font", font.length);
  const handle = new WebHandle();
  pump.handle = handle; pump.block = handle.block();
  window.echlubDemo = { handle, pump, report, storage };
  // The earlier visits' copies of unsaved work, offered by the app's recovery question.
  const copies = await storage.open();
  await handle.start($("app"), manifest, files, font, storage.ok ? storage.key : "", copies.map((c) => c[0]), copies.map((c) => c[1]));
  document.body.dataset.state = "running";
  pump.t0 = performance.now();
  setInterval(tick, 10);
  setInterval(showAudio, 250);
  let asks = 0;
  setInterval(checkPick, 100);
  setInterval(() => {
    const n = Number(handle.unavailable_asks());
    if (n > asks) toast("Importing audio, samples and wavetables, and exporting stems, are not available in the browser. Projects and MIDI files open and save from the File menu; the mix exports as WAV or FLAC.", null, 7000);
    saveDownloads();
    drainStore().catch(() => {});
    asks = n;
    let dead = true;
    try { dead = handle.has_panicked(); } catch (_) { /* the wasm instance aborted */ }
    if (dead && !report.panicked) {
      $("audio").textContent = "The app stopped after an internal error. Reload the page to start again.";
      toast("The app stopped after an internal error. Reload the page to start again.");
    }
    setReport("panicked", dead);
    setReport("fileAsks", n);
  }, 300);
  $("sound").addEventListener("click", () => startSound().catch((e) => toast(`Sound could not start: ${e.message}`)));
  // A browser plays sound only after a click on the page: ask once, up
  // front, instead of leaving it to the small button in the bar
  // (`?gate=0` skips it, for the headless checks).
  const gate = $("gate");
  const closeGate = () => { gate.hidden = true; };
  $("gate-on").addEventListener("click", () => {
    closeGate();
    if (pump.mode !== "audio") startSound().catch((e) => toast(`Sound could not start: ${e.message}`));
  });
  $("gate-off").addEventListener("click", () => { closeGate(); toast("Sound is off. Start it any time with the button at the bottom left."); });
  gate.addEventListener("keydown", (e) => { if (e.key === "Escape") $("gate-off").click(); });
  if (params.get("gate") !== "0") { gate.hidden = false; $("gate-on").focus(); }
}

main().catch((e) => fail(e && e.message ? e.message : e));
