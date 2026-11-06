interface PlaybackSettingsValue { speed: number; skip_intro: number; skip_outro: number }

/** Ignore stale acknowledgements until the most recent full edit is observed. */
export function acceptSettingsSnapshot(pending: PlaybackSettingsValue | null, snapshot: PlaybackSettingsValue, folderChanged: boolean): boolean {
  return folderChanged || pending === null || (pending.speed === snapshot.speed
    && pending.skip_intro === snapshot.skip_intro && pending.skip_outro === snapshot.skip_outro);
}

/** Estimate original-audio time between backend snapshots. */
export function elapsedPosition(position: number, elapsedMs: number, playing: boolean, speed: number): number {
  return playing ? position + Math.max(0, elapsedMs) * speed / 1000 : position;
}

/** Match the backend's finite, unsigned whole-second limit. */
export function parseSkipSeconds(value: string): number | null {
  if (value.trim() === "") return null;
  const seconds = Number(value);
  return Number.isFinite(seconds) && Number.isInteger(seconds) && seconds >= 0 && seconds <= 4294967295
    ? seconds : null;
}
