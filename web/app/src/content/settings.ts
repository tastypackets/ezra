import type { ReleaseChannel } from "@ezra/client";

export const SETTINGS_DESCRIPTIONS = {
  claude_description: "How the manager installs and updates Claude Code.",
  release_channel: "Release channel",
  release_channel_hint:
    "Switching to stable keeps the installed version until stable has a newer one.",
  save: "Save",
  saved: "Saved",
} as const;

export const RELEASE_CHANNELS: Record<ReleaseChannel, { title: string; description: string }> = {
  latest: {
    title: "Latest",
    description: "Every release as soon as it ships, Claude Code's default.",
  },
  stable: {
    title: "Stable",
    description: "About a week behind, skipping releases with major regressions.",
  },
};
