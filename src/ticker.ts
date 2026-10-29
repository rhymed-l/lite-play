/** How far (px) to shift one line of subtitle text to the left at playback `position`.
 *
 * The line never wraps. When it is wider than the window it scrolls, timed so that the whole
 * line has been shown by the time the cue ends: the start is held still briefly so the first
 * words can be read, and the end is reached a little before the cue is over.
 */
const HOLD_START = 0.15;
const HOLD_END = 0.1;

export function scrollOffset(
  textWidth: number,
  boxWidth: number,
  cueStart: number,
  cueEnd: number,
  position: number,
): number {
  if (!Number.isFinite(textWidth) || !Number.isFinite(boxWidth) || boxWidth <= 0) return 0;
  if (!Number.isFinite(position) || !Number.isFinite(cueStart) || !Number.isFinite(cueEnd)) return 0;
  const overflow = textWidth - boxWidth;
  if (overflow <= 0) return 0;

  const length = cueEnd - cueStart;
  if (length <= 0) return overflow;

  const progress = (position - cueStart) / length;
  if (progress <= HOLD_START) return 0;
  if (progress >= 1 - HOLD_END) return overflow;
  return overflow * ((progress - HOLD_START) / (1 - HOLD_START - HOLD_END));
}

export interface SchemeInput {
  scheme: string;
  custom_fg: string;
  custom_bg: string;
}

/** Text colour of a scheme. The desktop subtitle has no background of its own: it is always
 * drawn straight onto the desktop, so only the text colour varies. */
export function colours(s: SchemeInput): { fg: string } {
  switch (s.scheme) {
    case "light":
      return { fg: "#111111" };
    case "yellow":
      return { fg: "#ffe14d" };
    case "custom":
      return { fg: s.custom_fg };
    default:
      return { fg: "#ffffff" };
  }
}
