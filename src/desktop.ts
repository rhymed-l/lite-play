import "./desktop.css";
import {
  getSnapshot,
  getSubtitle,
  getSubtitleStatus,
  onSnapshot,
  onSubtitleActive,
  onSubtitleSettings,
  onSubtitleTrack,
  type Cue,
  type SubtitleSettings,
} from "./api";
import { colours, scrollOffset } from "./ticker";

const frame = document.getElementById("frame") as HTMLDivElement;
const line = document.getElementById("line") as HTMLParagraphElement;

let cues: Cue[] = [];
let active: number | null = null;
let position = 0;
let playing = false;
let positionAt = performance.now();
let raf = 0;

function applySettings(s: SubtitleSettings): void {
  const { fg } = colours(s);
  const root = document.documentElement;
  root.style.setProperty("--sub-fg", fg);
  // The window is sized for this font; CSS reads it from here, not from a hard-coded value.
  root.style.setProperty("--sub-size", `${s.font_size}px`);
  frame.classList.toggle("unlocked", !s.desktop_locked);
  // Locked means click-through: nothing in the page may pretend to be draggable.
  if (s.desktop_locked) frame.removeAttribute("data-tauri-drag-region");
  else frame.setAttribute("data-tauri-drag-region", "");
}

/** Playback position now, estimated between the engine's updates (it reports every 0.5 s). */
function now(): number {
  return playing ? position + (performance.now() - positionAt) / 1000 : position;
}

function render(): void {
  const cue = active !== null ? cues[active] : undefined;
  const text = cue?.text ?? "";
  if (line.textContent !== text) line.textContent = text;
  frame.classList.toggle("empty", text === "");
  if (!cue) {
    line.style.transform = "";
    return;
  }
  const offset = scrollOffset(line.scrollWidth, frame.clientWidth - 32, cue.start, cue.end, now());
  line.style.transform = offset > 0 ? `translateX(${-offset}px)` : "";
}

function tick(): void {
  render();
  raf = requestAnimationFrame(tick);
}

function onPosition(p: number, isPlaying: boolean): void {
  position = p;
  playing = isPlaying;
  positionAt = performance.now();
}

async function start(): Promise<void> {
  await onSubtitleSettings(applySettings);
  await onSubtitleTrack((t) => {
    cues = t.cues;
    active = null;
    render();
  });
  await onSubtitleActive((index) => {
    active = index;
    render();
  });
  await onSnapshot((s) => onPosition(s.position, s.playing));

  const status = await getSubtitleStatus();
  applySettings(status.settings);
  const [track, index] = await getSubtitle();
  cues = track.cues;
  active = index;
  const snap = await getSnapshot();
  if (snap) onPosition(snap.position, snap.playing);
  render();
  if (!raf) raf = requestAnimationFrame(tick);
}

void start();
