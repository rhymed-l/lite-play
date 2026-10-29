import { open } from "@tauri-apps/plugin-dialog";
import "./style.css";
import { initSubtitles } from "./subtitles";
import {
  addFolder,
  getSnapshot,
  nextTrack,
  onSnapshot,
  playIndex,
  playPause,
  prevTrack,
  seekTo,
  selectFolder,
  setDirection,
  setMemoryEnabled,
  setMode,
  setVolume,
  type Mode,
  type Snapshot,
} from "./api";

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}

const errorBox = el<HTMLDivElement>("error");
const folderSelect = el<HTMLSelectElement>("folder-select");
const addFolderBtn = el<HTMLButtonElement>("add-folder");
const trackList = el<HTMLUListElement>("tracks");
const seek = el<HTMLInputElement>("seek");
const timeLabel = el<HTMLSpanElement>("time");
const prevBtn = el<HTMLButtonElement>("prev");
const playBtn = el<HTMLButtonElement>("play");
const nextBtn = el<HTMLButtonElement>("next");
const volume = el<HTMLInputElement>("volume");
const modeSelect = el<HTMLSelectElement>("mode");
const directionBtn = el<HTMLButtonElement>("direction");
const memoryBox = el<HTMLInputElement>("memory");

let latest: Snapshot | null = null;
let seeking = false;
let draggingVolume = false;
let renderedTracksKey = "";
let renderedFoldersKey = "";
let renderedCurrent: number | null = null;

function formatTime(secs: number | null): string {
  if (secs === null || !Number.isFinite(secs)) return "--:--";
  const total = Math.max(0, Math.floor(secs));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function renderFolders(s: Snapshot): void {
  const key = JSON.stringify([s.folders, s.folder]);
  if (key === renderedFoldersKey) return;
  renderedFoldersKey = key;
  folderSelect.replaceChildren();
  if (s.folders.length === 0) {
    const none = document.createElement("option");
    none.textContent = "还没有文件夹";
    none.disabled = true;
    none.selected = true;
    folderSelect.append(none);
    folderSelect.disabled = true;
    return;
  }
  folderSelect.disabled = false;
  if (s.folder === null) {
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = "选择文件夹…";
    placeholder.disabled = true;
    placeholder.selected = true;
    folderSelect.append(placeholder);
  }
  for (const f of s.folders) {
    const option = document.createElement("option");
    option.value = f;
    option.textContent = baseName(f);
    option.title = f;
    option.selected = f === s.folder;
    folderSelect.append(option);
  }
}

function renderTracks(s: Snapshot): void {
  const key = JSON.stringify(s.tracks.map((t) => [t.path, t.unavailable]));
  if (key !== renderedTracksKey) {
    renderedTracksKey = key;
    renderedCurrent = null;
    trackList.replaceChildren();
    if (s.tracks.length === 0) {
      const empty = document.createElement("li");
      empty.className = "empty";
      empty.textContent = s.folder ? "这个文件夹里没有音频文件" : "点击“添加文件夹”开始";
      trackList.append(empty);
      return;
    }
    s.tracks.forEach((track, idx) => {
      const item = document.createElement("li");
      item.textContent = track.name;
      item.title = track.path;
      item.tabIndex = 0;
      if (track.unavailable) item.classList.add("unavailable");
      item.addEventListener("dblclick", () => void playIndex(idx));
      item.addEventListener("keydown", (e) => {
        if (e.key === "Enter") void playIndex(idx);
      });
      trackList.append(item);
    });
  }
  if (renderedCurrent !== s.current) {
    renderedCurrent = s.current;
    const items = trackList.children;
    for (let i = 0; i < items.length; i++) {
      items[i].classList.toggle("current", i === s.current);
    }
    if (s.current !== null) {
      items[s.current]?.scrollIntoView({ block: "nearest" });
    }
  }
}

function render(s: Snapshot): void {
  latest = s;

  errorBox.hidden = s.error === null;
  const errorText = s.error ?? "";
  if (errorBox.textContent !== errorText) errorBox.textContent = errorText;

  renderFolders(s);
  renderTracks(s);

  playBtn.textContent = s.playing ? "暂停" : "播放";
  playBtn.setAttribute("aria-label", s.playing ? "暂停" : "播放");

  const hasDuration = s.duration !== null && s.duration > 0;
  seek.disabled = s.current === null;
  seek.max = String(hasDuration ? Math.floor(s.duration as number) : Math.max(1, Math.floor(s.position)));
  if (!seeking) {
    seek.value = String(Math.floor(s.position));
    timeLabel.textContent = `${formatTime(s.position)} / ${formatTime(s.duration)}`;
  }

  if (!draggingVolume) volume.value = String(Math.round(s.volume * 100));
  modeSelect.value = s.mode;
  directionBtn.setAttribute("aria-pressed", String(s.direction === "Reverse"));
  directionBtn.textContent = s.direction === "Reverse" ? "倒序" : "正序";
  memoryBox.checked = s.memory_enabled;
}

addFolderBtn.addEventListener("click", async () => {
  const picked = await open({ directory: true, multiple: false });
  if (typeof picked === "string") await addFolder(picked);
});

folderSelect.addEventListener("change", () => {
  if (folderSelect.value) void selectFolder(folderSelect.value);
});

playBtn.addEventListener("click", () => void playPause());
prevBtn.addEventListener("click", () => void prevTrack());
nextBtn.addEventListener("click", () => void nextTrack());

seek.addEventListener("input", () => {
  seeking = true;
  timeLabel.textContent = `${formatTime(Number(seek.value))} / ${formatTime(latest?.duration ?? null)}`;
});
seek.addEventListener("change", () => {
  seeking = false;
  void seekTo(Number(seek.value));
});

volume.addEventListener("pointerdown", () => {
  draggingVolume = true;
});
volume.addEventListener("input", () => void setVolume(Number(volume.value) / 100));
window.addEventListener("pointerup", () => {
  draggingVolume = false;
});

modeSelect.addEventListener("change", () => void setMode(modeSelect.value as Mode));
directionBtn.addEventListener("click", () => {
  const reversed = latest?.direction === "Reverse";
  void setDirection(reversed ? "Forward" : "Reverse");
});
memoryBox.addEventListener("change", () => void setMemoryEnabled(memoryBox.checked));

async function start(): Promise<void> {
  await onSnapshot(render);
  const first = await getSnapshot();
  if (first && latest === null) render(first);
  await initSubtitles();
}

void start();
