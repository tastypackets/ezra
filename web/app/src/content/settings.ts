import type { ReleaseChannel } from "@ezra/client";

export const SETTINGS_DESCRIPTIONS = {
  release_channel: "Release channel",
  release_channel_hint:
    "Switching to stable keeps the installed version until stable has a newer one.",
  remote_control: "Remote Control",
  remote_control_enabled: "Serve to the Claude app",
  remote_control_enabled_hint:
    "Serves /projects, and the folders you choose, while Claude Code is signed in.",
  serve_repositories: "Serve new repositories",
  serve_repositories_hint: "Repositories added to /projects start with their switch on.",
  permission_mode: "Permission mode",
  permission_mode_hint: "New sessions from the Claude app start in this mode.",
  permission_mode_unknown: "Choose one of the listed modes.",
  show_permission_modes: "Show permission modes",
  capacity: "Sessions per folder",
  capacity_hint: "Each session is its own Claude Code process.",
  capacity_default: "Claude Code's default",
  capacity_range: "Enter a whole number of 1 or more, or leave it empty.",
  save: "Save",
  saved: "Claude Code settings saved.",
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
  { value: "plan", description: "Reads, plus commands auto mode's checks allow." },
  { value: "default", description: "Reads only, asking before anything else." },
  { value: "dontAsk", description: "Reads and pre-approved tools, denying the rest." },
  { value: "bypassPermissions", description: "Everything but deny and ask rules, with no checks." },
];

/** Every mode Claude Code takes, `manual` being another name for `default`. */
export const PERMISSION_MODE_NAMES: ReadonlySet<string> = new Set([
  ...PERMISSION_MODES.map(({ value }) => value),
  "manual",
]);
