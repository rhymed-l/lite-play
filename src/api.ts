import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Mode = "Sequential" | "Single" | "Shuffle";
export type Direction = "Forward" | "Reverse";

export interface TrackInfo {
  path: string;
  name: string;
  unavailable: boolean;
}

export interface Snapshot {
  folder: string | null;
  folders: string[];
  tracks: TrackInfo[];
  current: number | null;
  playing: boolean;
  position: number;
  duration: number | null;
  mode: Mode;
  direction: Direction;
  volume: number;
  memory_enabled: boolean;
  error: string | null;
}

export const playPause = () => invoke<void>("play_pause");
export const nextTrack = () => invoke<void>("next_track");
export const prevTrack = () => invoke<void>("prev_track");
export const playIndex = (idx: number) => invoke<void>("play_index", { idx });
export const seekTo = (secs: number) => invoke<void>("seek_to", { secs });
export const setMode = (mode: Mode) => invoke<void>("set_mode", { mode });
export const setDirection = (direction: Direction) =>
  invoke<void>("set_direction", { direction });
export const setVolume = (volume: number) => invoke<void>("set_volume", { volume });
export const setMemoryEnabled = (enabled: boolean) =>
  invoke<void>("set_memory_enabled", { enabled });
export const addFolder = (path: string) => invoke<void>("add_folder", { path });
export const selectFolder = (path: string) => invoke<void>("select_folder", { path });
export const getSnapshot = () => invoke<Snapshot | null>("get_snapshot");

export interface Cue {
  start: number;
  end: number;
  text: string;
}

export type SubtitleFileStatus = "none" | "ready" | "unreadable";

export interface TrackSubtitles {
  path: string | null;
  status: SubtitleFileStatus;
  cues: Cue[];
}

export interface DesktopPos {
  x: number;
  y: number;
  width: number;
  height: number;
}

import type { PowerInfo, PowerName } from "./power";

export interface SubtitleSettings {
  power: PowerName;
  enabled: boolean;
  desktop_enabled: boolean;
  desktop_locked: boolean;
  font_size: number;
  scheme: string;
  custom_fg: string;
  custom_bg: string;
  desktop_pos: DesktopPos | null;
}

export type UpdateState =
  | "NotInstalled"
  | "UpToDate"
  | { Available: { runtime: boolean; model: boolean; vad: boolean } };

export interface SubtitleProgress {
  done: number;
  total: number;
  /** File names of the episodes being worked on right now. */
  current: string[];
  failed: string[];
  running: boolean;
}

export interface SubtitleStatus {
  supported: boolean;
  installed: boolean;
  installing: boolean;
  update: UpdateState;
  settings: SubtitleSettings;
  progress: SubtitleProgress | null;
}

export interface InstallEvent {
  stage: string;
  done: number;
  total: number;
  message: string | null;
}

export const getSubtitleStatus = () => invoke<SubtitleStatus>("get_subtitle_status");
export const getSubtitle = () =>
  invoke<[TrackSubtitles, number | null]>("get_subtitle");
export const setSubtitleEnabled = (enabled: boolean) =>
  invoke<void>("set_subtitle_enabled", { enabled });
export const installSubtitleAddon = () => invoke<void>("install_subtitle_addon");
export const cancelSubtitleInstall = () => invoke<void>("cancel_subtitle_install");
export const checkSubtitleUpdates = () => invoke<void>("check_subtitle_updates");
export const uninstallSubtitleAddon = () => invoke<void>("uninstall_subtitle_addon");
export const regenerateSubtitle = (path: string) =>
  invoke<void>("regenerate_subtitle", { path });
export const setSubtitleOptions = (
  fontSize: number,
  scheme: string,
  customFg: string,
  customBg: string,
) =>
  invoke<void>("set_subtitle_options", {
    fontSize,
    scheme,
    customFg,
    customBg,
  });
export const setSubtitlePower = (power: PowerName) =>
  invoke<void>("set_subtitle_power", { power });
export const getPowerInfo = () => invoke<PowerInfo>("get_power_info");
export const setDesktopSubtitle = (enabled: boolean, locked: boolean) =>
  invoke<void>("set_desktop_subtitle", { enabled, locked });

export const onSubtitleTrack = (cb: (t: TrackSubtitles) => void): Promise<UnlistenFn> =>
  listen<TrackSubtitles>("subtitle-track", (e) => cb(e.payload));
export const onSubtitleActive = (cb: (index: number | null) => void): Promise<UnlistenFn> =>
  listen<{ index: number | null }>("subtitle-active", (e) => cb(e.payload.index));
export const onSubtitleProgress = (cb: (p: SubtitleProgress) => void): Promise<UnlistenFn> =>
  listen<SubtitleProgress>("subtitle-progress", (e) => cb(e.payload));
export const onSubtitleInstall = (cb: (e: InstallEvent) => void): Promise<UnlistenFn> =>
  listen<InstallEvent>("subtitle-install", (e) => cb(e.payload));
export const onSubtitleUpdate = (cb: (u: UpdateState) => void): Promise<UnlistenFn> =>
  listen<UpdateState>("subtitle-update", (e) => cb(e.payload));
export const onSubtitleSettings = (cb: (s: SubtitleSettings) => void): Promise<UnlistenFn> =>
  listen<SubtitleSettings>("subtitle-settings", (e) => cb(e.payload));

export function onSnapshot(cb: (s: Snapshot) => void): Promise<UnlistenFn> {
  return listen<Snapshot>("snapshot", (e) => cb(e.payload));
}
