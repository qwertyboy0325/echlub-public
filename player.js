// The AudioWorklet player: plays the blocks the main thread renders and
// posts here, 128 frames a quantum, and reports what it consumed, what is
// queued and how many quanta it had to fill with silence (underruns).
// It also hashes the first `hashFrames` frames of blocks rendered while the
// transport played: FNV-1a 32-bit over the f32 samples' little-endian
// bytes, the same function as the native reference (src/pump.rs PlayHash).
class Player extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.hashWant = (options.processorOptions && options.processorOptions.hashFrames) || 0;
    this.reset();
    this.port.onmessage = (e) => {
      const d = e.data;
      if (d.reset) { this.reset(); return; }
      this.queue.push(d.samples);
      this.queued += d.samples.length / 2;
      this.feedHash(d.samples, d.flags, d.block);
    };
  }

  reset() {
    this.queue = []; this.head = null; this.at = 0; this.queued = 0; this.consumed = 0;
    this.underruns = 0; this.missing = 0; this.started = false; this.since = 0;
    this.hash = 0x811c9dc5; this.hashFrames = 0; this.hashStarted = false; this.hashEnded = false; this.nonzero = 0;
  }

  feedHash(samples, flags, block) {
    if (!this.hashWant) return;
    const bytes = new Uint8Array(samples.buffer, samples.byteOffset, samples.byteLength);
    for (let b = 0; b < flags.length; b++) {
      if (this.hashFrames >= this.hashWant || this.hashEnded) return;
      if (!flags[b]) { if (this.hashStarted) this.hashEnded = true; continue; }
      this.hashStarted = true;
      const take = Math.min(block, this.hashWant - this.hashFrames);
      let h = this.hash;
      for (let i = b * block * 2; i < (b * block + take) * 2; i++) if (samples[i] !== 0) this.nonzero++;
      const end = (b * block + take) * 8;
      for (let i = b * block * 8; i < end; i++) { h ^= bytes[i]; h = Math.imul(h, 0x01000193) >>> 0; }
      this.hash = h;
      this.hashFrames += take;
    }
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    const l = out[0], r = out[1] || out[0];
    const n = l.length;
    let i = 0;
    while (i < n) {
      if (!this.head) {
        this.head = this.queue.shift() || null;
        this.at = 0;
        if (!this.head) break;
      }
      const h = this.head;
      while (i < n && this.at < h.length) { l[i] = h[this.at]; r[i] = h[this.at + 1]; this.at += 2; i++; }
      if (this.at >= h.length) this.head = null;
    }
    this.consumed += i; this.queued -= i;
    if (i > 0) this.started = true;
    if (i < n) {
      for (let k = i; k < n; k++) { l[k] = 0; r[k] = 0; }
      if (this.started) { this.underruns++; this.missing += n - i; }
    }
    this.since += n;
    if (this.since >= 2400) {
      this.since = 0;
      this.port.postMessage({
        consumed: this.consumed, queued: this.queued, underruns: this.underruns, missing: this.missing,
        hash: this.hashFrames ? (this.hash >>> 0).toString(16).padStart(8, "0") : null,
        hashFrames: this.hashFrames, hashDone: this.hashFrames >= this.hashWant, nonzero: this.nonzero,
      });
    }
    return true;
  }
}
registerProcessor("echlub-player", Player);
