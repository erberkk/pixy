// Runs on the audio rendering thread, so it is loaded as a worklet module by
// mic.js rather than imported like every other file here — it shares no scope
// with the rest of the app and cannot import from it.
//
// Its only job is repackaging. The audio thread hands us 128 frames at a time,
// but openWakeWord's chain works in 1280-sample (80ms) hops, and the ONNX
// runtime lives on the main thread. Doing the regrouping here rather than in
// the main-thread handler means one message per hop instead of ten, and no
// partial-chunk bookkeeping on the other side.
const HOP = 1280;

class CaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(HOP);
    this.filled = 0;
  }

  process(inputs) {
    const channel = inputs[0]?.[0];
    // No input connected yet, or the device dropped out mid-stream. Returning
    // true (rather than false) keeps the processor alive so capture resumes by
    // itself if the device comes back.
    if (!channel) return true;

    for (let i = 0; i < channel.length; i++) {
      this.buf[this.filled++] = channel[i];
      if (this.filled === HOP) {
        // A copy, not this.buf itself: the buffer is reused for the next hop,
        // and a transfer would leave us with a detached array.
        this.port.postMessage(this.buf.slice());
        this.filled = 0;
      }
    }
    return true;
  }
}

registerProcessor("capture-processor", CaptureProcessor);
