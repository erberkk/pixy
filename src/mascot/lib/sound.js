// Short WebAudio blips for state changes. Synthesised rather than shipping
// audio files — they are two-oscillator beeps and this keeps the bundle empty.

let audioCtx = null;

function getAudioCtx() {
  if (!audioCtx) {
    audioCtx = new (window.AudioContext || window.webkitAudioContext)();
  }
  return audioCtx;
}

export function beep({ freq, duration, gain = 0.05, delay = 0 }) {
  const ctx = getAudioCtx();
  if (ctx.state === "suspended") {
    ctx.resume();
  }
  const startAt = ctx.currentTime + delay;

  const oscillator = ctx.createOscillator();
  const gainNode = ctx.createGain();

  oscillator.type = "sine";
  oscillator.frequency.setValueAtTime(freq, startAt);

  gainNode.gain.setValueAtTime(0, startAt);
  gainNode.gain.linearRampToValueAtTime(gain, startAt + 0.01);
  gainNode.gain.linearRampToValueAtTime(0, startAt + duration);

  oscillator.connect(gainNode);
  gainNode.connect(ctx.destination);

  oscillator.start(startAt);
  oscillator.stop(startAt + duration + 0.02);
}

export function playSound(state) {
  switch (state) {
    case "waiting_permission":
      // urgent double beep: Claude needs an explicit yes/no decision
      beep({ freq: 880, duration: 0.15, gain: 0.2 });
      beep({ freq: 880, duration: 0.15, gain: 0.2, delay: 0.2 });
      break;
    case "waiting_input":
      // softer single chime: Claude is idle, waiting for your next message
      beep({ freq: 660, duration: 0.2, gain: 0.15 });
      break;
    case "turn_done":
      // quiet tick: Claude just finished a turn
      beep({ freq: 440, duration: 0.12, gain: 0.1 });
      break;
    default:
      break;
  }
}
