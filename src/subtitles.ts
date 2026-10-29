import { powerHint, type PowerInfo, type PowerName } from "./power";
import {
  getPowerInfo,
  setSubtitlePower,
  cancelSubtitleInstall,
  checkSubtitleUpdates,
  getSubtitle,
  getSubtitleStatus,
  installSubtitleAddon,
  onSubtitleActive,
  onSubtitleInstall,
  onSubtitleProgress,
  onSubtitleSettings,
  onSubtitleTrack,
  onSubtitleUpdate,
  seekTo,
  setDesktopSubtitle,
  setSubtitleEnabled,
  setSubtitleOptions,
  uninstallSubtitleAddon,
  type SubtitleProgress,
  type SubtitleSettings,
  type SubtitleStatus,
  type TrackSubtitles,
  type UpdateState,
} from "./api";

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}

const tabList = el<HTMLButtonElement>("tab-list");
const tabSubtitles = el<HTMLButtonElement>("tab-subtitles");
const tracksPanel = el<HTMLUListElement>("tracks");
const subtitlePanel = el<HTMLDivElement>("subtitle-view");
const notice = el<HTMLParagraphElement>("subtitle-notice");
const cueList = el<HTMLOListElement>("cue-list");

const unsupported = el<HTMLParagraphElement>("sub-unsupported");
const enabledBox = el<HTMLInputElement>("sub-enabled");
const progressText = el<HTMLParagraphElement>("sub-progress");
const failedText = el<HTMLParagraphElement>("sub-failed");
const installRow = el<HTMLDivElement>("sub-install");
const installBar = el<HTMLProgressElement>("sub-install-bar");
const installLabel = el<HTMLSpanElement>("sub-install-label");
const installCancel = el<HTMLButtonElement>("sub-install-cancel");
const installError = el<HTMLParagraphElement>("sub-install-error");
const updateBtn = el<HTMLButtonElement>("sub-update");
const uninstallBtn = el<HTMLButtonElement>("sub-uninstall");
const fontRange = el<HTMLInputElement>("sub-font");
const fontValue = el<HTMLSpanElement>("sub-font-value");
const schemeSelect = el<HTMLSelectElement>("sub-scheme");
const fgInput = el<HTMLInputElement>("sub-fg");
const bgInput = el<HTMLInputElement>("sub-bg");
const desktopBox = el<HTMLInputElement>("sub-desktop");
const lockedBox = el<HTMLInputElement>("sub-locked");

const powerSelect = el<HTMLSelectElement>("sub-power");
const powerHintEl = el<HTMLParagraphElement>("sub-power-hint");
let powerInfo: PowerInfo | null = null;

function renderPower(level: PowerName): void {
  powerSelect.value = level;
  powerHintEl.textContent = powerInfo ? powerHint(powerInfo, level) : "";
}

/** Reads the machine's cores and free memory again, so the hint is true at the moment of looking. */
async function refreshPowerInfo(level: PowerName): Promise<void> {
  try {
    powerInfo = await getPowerInfo();
  } catch {
    powerInfo = null;
  }
  renderPower(level);
}

const DOWNLOAD_NOTE = "字幕组件需要从 GitHub 下载约 180MB，是否继续？";

let status: SubtitleStatus | null = null;
let track: TrackSubtitles = { path: null, status: "none", cues: [] };
let activeIndex: number | null = null;
let installing = false;
let tab: "list" | "subtitles" = "list";

function hasUpdate(u: UpdateState): boolean {
  return typeof u === "object" && "Available" in u;
}

/** Why there is nothing to show, in the words the user should see. */
function emptyReason(): string | null {
  if (status === null) return null;
  if (!status.supported) return "字幕功能目前只支持 Windows";
  if (!status.installed) return "字幕组件还没安装";
  if (!status.settings.enabled) return "字幕已关闭";
  if (track.status === "unreadable") return "字幕文件无法读取";
  if (track.status === "none") {
    if (track.path === null) return "没有正在播放的曲目";
    return status.progress?.running ? "字幕生成中…" : "本集还没有字幕";
  }
  return null;
}

function renderCues(): void {
  cueList.replaceChildren();
  track.cues.forEach((cue, idx) => {
    const item = document.createElement("li");
    item.textContent = cue.text;
    item.tabIndex = 0;
    item.addEventListener("click", () => void seekTo(cue.start));
    item.addEventListener("keydown", (e) => {
      if (e.key === "Enter") void seekTo(cue.start);
    });
    if (idx === activeIndex) item.classList.add("active");
    cueList.append(item);
  });
}

function highlight(): void {
  const items = cueList.children;
  for (let i = 0; i < items.length; i++) {
    items[i].classList.toggle("active", i === activeIndex);
  }
  if (activeIndex !== null && tab === "subtitles") {
    items[activeIndex]?.scrollIntoView({ block: "center" });
  }
}

function renderNotice(): void {
  const reason = emptyReason();
  notice.hidden = reason === null;
  notice.replaceChildren();
  if (reason === null) return;
  notice.append(document.createTextNode(reason));
  if (status?.supported && !status.installed) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "安装字幕组件";
    button.disabled = installing;
    button.addEventListener("click", () => void startInstall());
    notice.append(document.createElement("br"), button);
  }
}

function showTab(next: "list" | "subtitles"): void {
  tab = next;
  tabList.setAttribute("aria-selected", String(next === "list"));
  tabSubtitles.setAttribute("aria-selected", String(next === "subtitles"));
  tracksPanel.hidden = next !== "list";
  subtitlePanel.hidden = next !== "subtitles";
  if (next === "subtitles") highlight();
}

function progressSummary(p: SubtitleProgress | null): string {
  if (!p || p.total === 0) return "";
  const base = `已生成 ${p.done} / 共 ${p.total} 集`;
  if (!p.running || p.current.length === 0) return base;
  // Many episodes at once: name the first couple, count the rest.
  const shown = p.current.slice(0, 2).join("、");
  const more = p.current.length > 2 ? ` 等 ${p.current.length} 集` : "";
  return `${base}，正在处理：${shown}${more}`;
}

function renderSettings(): void {
  if (status === null) return;
  const s = status.settings;
  unsupported.hidden = status.supported;
  enabledBox.disabled = !status.supported || installing;
  enabledBox.checked = s.enabled;
  progressText.textContent = progressSummary(status.progress);
  const failed = status.progress?.failed ?? [];
  failedText.hidden = failed.length === 0;
  failedText.textContent = failed.length ? `这些集没有生成成功：${failed.join("、")}` : "";

  installRow.hidden = !installing;
  updateBtn.hidden = !(status.installed && hasUpdate(status.update)) || installing;
  uninstallBtn.hidden = !status.installed || installing;

  fontRange.value = String(s.font_size);
  fontValue.textContent = String(s.font_size);
  schemeSelect.value = s.scheme;
  fgInput.value = s.custom_fg;
  bgInput.value = s.custom_bg;
  fgInput.hidden = bgInput.hidden = s.scheme !== "custom";
  desktopBox.checked = s.desktop_enabled;
  lockedBox.checked = s.desktop_locked;
  lockedBox.disabled = !s.desktop_enabled;
  renderPower(s.power);
}

function renderAll(): void {
  renderNotice();
  renderSettings();
}

async function startInstall(): Promise<void> {
  if (installing) return;
  if (!window.confirm(DOWNLOAD_NOTE)) {
    enabledBox.checked = false;
    return;
  }
  installError.hidden = true;
  installing = true;
  installBar.removeAttribute("value");
  installLabel.textContent = "准备中…";
  renderAll();
  try {
    await installSubtitleAddon();
  } catch (e) {
    finishInstall(String(e));
  }
}

function finishInstall(message: string | null): void {
  installing = false;
  installError.hidden = message === null;
  installError.textContent = message ?? "";
  void refreshStatus();
}

const STAGE_NAMES: Record<string, string> = {
  runtime: "下载识别程序",
  model: "下载语音模型",
  vad: "下载语音检测文件",
  selftest: "检查组件能否运行",
};

function onInstall(e: { stage: string; done: number; total: number; message: string | null }): void {
  if (e.stage === "done") return finishInstall(null);
  if (e.stage === "error") return finishInstall(e.message ?? "安装失败");
  const name = STAGE_NAMES[e.stage] ?? e.stage;
  if (e.total > 0) {
    installBar.max = e.total;
    installBar.value = e.done;
    const pct = Math.floor((e.done / e.total) * 100);
    installLabel.textContent = `${name} ${pct}%`;
  } else {
    installBar.removeAttribute("value");
    installLabel.textContent = name;
  }
}

async function refreshStatus(): Promise<void> {
  status = await getSubtitleStatus();
  installing = installing || status.installing;
  renderAll();
}

function pushOptions(): void {
  const scheme = schemeSelect.value;
  void setSubtitleOptions(Number(fontRange.value), scheme, fgInput.value, bgInput.value);
}

function applySettings(s: SubtitleSettings): void {
  if (status) status.settings = s;
  renderAll();
}

/** Left/Right arrows move between the two tabs, as a tab list should. */
function tablistKeys(): void {
  for (const button of [tabList, tabSubtitles]) {
    button.addEventListener("keydown", (e) => {
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      const next = button === tabList ? tabSubtitles : tabList;
      next.click();
      next.focus();
    });
  }
}

export async function initSubtitles(): Promise<void> {
  tabList.addEventListener("click", () => showTab("list"));
  tabSubtitles.addEventListener("click", () => showTab("subtitles"));
  tablistKeys();

  enabledBox.addEventListener("change", async () => {
    if (!enabledBox.checked) {
      await setSubtitleEnabled(false);
      return;
    }
    if (status && !status.installed) return void startInstall();
    try {
      await setSubtitleEnabled(true);
    } catch (e) {
      enabledBox.checked = false;
      installError.hidden = false;
      installError.textContent = String(e);
    }
  });
  installCancel.addEventListener("click", () => void cancelSubtitleInstall());
  updateBtn.addEventListener("click", () => void startInstall());
  uninstallBtn.addEventListener("click", async () => {
    if (!window.confirm("卸载后已生成的字幕文件仍会保留。确定卸载字幕组件？")) return;
    try {
      await uninstallSubtitleAddon();
    } catch (e) {
      installError.hidden = false;
      installError.textContent = String(e);
    }
    track = { path: track.path, status: "none", cues: [] };
    renderCues();
    await refreshStatus();
  });
  fontRange.addEventListener("input", () => {
    fontValue.textContent = fontRange.value;
    pushOptions();
  });
  powerSelect.addEventListener("change", () => {
    const level = powerSelect.value as PowerName;
    renderPower(level);
    void setSubtitlePower(level).then(() => refreshPowerInfo(level));
  });
  // Free memory changes all the time: look again whenever the panel is opened.
  const panel = document.getElementById("subtitle-settings");
  panel?.addEventListener("toggle", () => {
    if ((panel as HTMLDetailsElement).open) void refreshPowerInfo(powerSelect.value as PowerName);
  });
  schemeSelect.addEventListener("change", pushOptions);
  fgInput.addEventListener("change", pushOptions);
  bgInput.addEventListener("change", pushOptions);
  desktopBox.addEventListener("change", () => void setDesktopSubtitle(desktopBox.checked, lockedBox.checked));
  lockedBox.addEventListener("change", () => void setDesktopSubtitle(desktopBox.checked, lockedBox.checked));

  await onSubtitleTrack((t) => {
    track = t;
    renderCues();
    renderNotice();
  });
  await onSubtitleActive((index) => {
    activeIndex = index;
    highlight();
  });
  await onSubtitleProgress((p) => {
    if (status) status.progress = p;
    progressText.textContent = progressSummary(p);
    failedText.hidden = p.failed.length === 0;
    failedText.textContent = p.failed.length ? `这些集没有生成成功：${p.failed.join("、")}` : "";
    renderNotice();
  });
  await onSubtitleInstall(onInstall);
  await onSubtitleUpdate((u) => {
    if (status) status.update = u;
    renderSettings();
  });
  await onSubtitleSettings(applySettings);

  await refreshStatus();
  await refreshPowerInfo(status?.settings.power ?? "light");
  const [current, active] = await getSubtitle();
  track = current;
  activeIndex = active;
  renderCues();
  renderAll();
  void checkSubtitleUpdates();
  showTab("list");
}
