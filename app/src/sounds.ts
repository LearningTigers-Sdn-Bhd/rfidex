import type { VerifyState } from "./api";

const MUTE_KEY = "rfidex.verify.muted";
const VOLUME = 0.5;

let ctx: AudioContext | null = null;

export function isMuted(): boolean {
  try {
    return localStorage.getItem(MUTE_KEY) === "1";
  } catch {
    return false;
  }
}

export function setMuted(muted: boolean) {
  try {
    localStorage.setItem(MUTE_KEY, muted ? "1" : "0");
  } catch {
    // The choice just is not remembered.
  }
}

/** Made on first use, which is after the operator has clicked into the tab. */
function audio(): AudioContext | null {
  try {
    ctx ??= new AudioContext();
    if (ctx.state === "suspended") void ctx.resume();
    return ctx;
  } catch {
    return null;
  }
}

/** One note: fades in fast and out slowly so it never clicks. */
function note(
  c: AudioContext,
  { at, from, to = from, length, type = "sine", level, lowpass }: {
    at: number; from: number; to?: number; length: number; type?: OscillatorType; level: number; lowpass?: number;
  },
) {
  const start = c.currentTime + at;
  const osc = c.createOscillator();
  const gain = c.createGain();
  osc.type = type;
  osc.frequency.setValueAtTime(from, start);
  if (to !== from) osc.frequency.exponentialRampToValueAtTime(to, start + length);
  gain.gain.setValueAtTime(0.0001, start);
  gain.gain.exponentialRampToValueAtTime(level * VOLUME, start + 0.015);
  gain.gain.exponentialRampToValueAtTime(0.0001, start + length);
  osc.connect(gain);
  let out: AudioNode = gain;
  if (lowpass) {
    const filter = c.createBiquadFilter();
    filter.type = "lowpass";
    filter.frequency.value = lowpass;
    gain.connect(filter);
    out = filter;
  }
  out.connect(c.destination);
  osc.start(start);
  osc.stop(start + length + 0.05);
}

/** A short sound for each answer. Waiting is silent. */
export function playVerify(state: VerifyState) {
  if (isMuted() || state === "waiting") return;
  const c = audio();
  if (!c) return;
  switch (state) {
    case "verified": // bright and rising
      note(c, { at: 0, from: 784, length: 0.2, level: 0.35 });
      note(c, { at: 0.11, from: 1047, length: 0.34, level: 0.35 });
      break;
    case "invalid": // a firm double "no"
      note(c, { at: 0, from: 170, length: 0.15, type: "sawtooth", level: 0.3, lowpass: 700 });
      note(c, { at: 0.2, from: 170, length: 0.2, type: "sawtooth", level: 0.3, lowpass: 700 });
      break;
    case "unknown": // two clean falling notes: "not recognised"
      note(c, { at: 0, from: 620, length: 0.17, type: "triangle", level: 0.5 });
      note(c, { at: 0.2, from: 440, length: 0.3, type: "triangle", level: 0.5 });
      break;
    case "problem": // one low, steady tone
      note(c, { at: 0, from: 240, length: 0.6, level: 0.35 });
      break;
  }
}
