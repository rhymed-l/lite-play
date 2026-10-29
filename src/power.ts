export type PowerName = "light" | "balanced" | "full";

export interface PowerChoice {
  name: PowerName;
  /** Episodes made at once right now: the level, limited by free memory. */
  jobs: number;
  /** What the level alone would allow, ignoring memory. */
  wanted_jobs: number;
  threads_per_job: number;
}

export interface PowerInfo {
  cores: number;
  free_mb: number | null;
  choices: PowerChoice[];
}

const BUSY: Record<PowerName, string> = {
  light: "对电脑其它操作影响很小。",
  balanced: "会占用一部分电脑性能，同时做别的事可能略慢。",
  full: "会占满大部分电脑性能，同时做别的事可能变慢。",
};

/** One or two sentences under the speed dropdown, so the choice is not a blind one. */
export function powerHint(info: PowerInfo, level: PowerName): string {
  const choice = info.choices.find((c) => c.name === level);
  if (!choice) return "";

  const parts: string[] = [];
  parts.push(
    choice.jobs <= 1 ? "一次生成 1 集。" : `同时生成 ${choice.jobs} 集。`,
  );
  if (choice.wanted_jobs > choice.jobs) {
    parts.push(`受内存限制，这一档本来可以同时 ${choice.wanted_jobs} 集。`);
  }
  if (info.free_mb !== null && Number.isFinite(info.free_mb)) {
    parts.push(`当前可用内存约 ${(info.free_mb / 1024).toFixed(1)} GB。`);
  }
  parts.push(BUSY[level]);
  return parts.join("");
}
