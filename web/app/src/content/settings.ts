import type { ReleaseChannel } from "@ezra/client";

export const SETTINGS_DESCRIPTIONS = {
  claude_description: "How the manager installs, updates and serves Claude Code.",
  release_channel: "Release channel",
  release_channel_hint:
    "Switching to stable keeps the installed version until stable has a newer one.",
  remote_control: "Remote Control",
  remote_control_enabled: "Serve /projects",
  remote_control_enabled_hint:
    "Work in /projects from the Claude app and claude.ai/code while Claude Code is signed in.",
  permission_mode: "Permission mode",
  permission_mode_hint: "Sessions started from the Claude app keep this mode.",
  permission_mode_word: "Enter one word, such as auto.",
  show_permission_modes: "Show permission modes",
  capacity: "Sessions at once",
  capacity_hint: "Each session uses about 250 MB of memory.",
  capacity_range: "Enter a number from 1 to 32.",
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

export const PERMISSION_MODES: readonly { value: string; description: string }[] = [
  { value: "auto", description: "Everything, with background safety checks." },
  { value: "acceptEdits", description: "Reads, file edits and common file commands." },
  { value: "plan", description: "Reads, for exploring before changing anything." },
  { value: "default", description: "Reads only, asking before anything else." },
  { value: "dontAsk", description: "Reads and pre-approved tools, denying the rest." },
  { value: "bypassPermissions", description: "Everything, with no checks." },
];
