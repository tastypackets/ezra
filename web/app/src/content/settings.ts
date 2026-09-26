import type { ReleaseChannel } from "@ezra/client";

export const SETTINGS_DESCRIPTIONS = {
  release_channel: "Release channel",
  release_channel_hint:
    "Switching to stable keeps the installed version until stable has a newer one.",
  remote_control: "Remote Control",
  remote_control_enabled: "Serve to the Claude app",
  remote_control_enabled_hint:
    "Serves /projects, and the folders you choose, while Claude Code is signed in.",
  serve_repositories: "Serve repositories by default",
  serve_repositories_hint:
    "For repositories without their own choice. Each served folder runs its own Claude Code, which uses a few hundred MB of memory.",
  permission_mode: "Permission mode",
  permission_mode_hint: "Sessions started from the Claude app keep this mode.",
  permission_mode_word: "Enter one word, such as auto.",
  show_permission_modes: "Show permission modes",
  capacity: "Sessions at once",
  capacity_hint: "Each session runs another Claude Code, a few hundred MB more.",
  capacity_range: "Enter a number from 1 to 32.",
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
  { value: "plan", description: "Reads, for exploring before changing anything." },
  { value: "default", description: "Reads only, asking before anything else." },
  { value: "dontAsk", description: "Reads and pre-approved tools, denying the rest." },
  { value: "bypassPermissions", description: "Everything, with no checks." },
];
