import type { ServerState } from "@ezra/client";

export const REMOTE_CONTROL_DESCRIPTIONS = {
  title: "Remote Control",
  open: "Open claude.ai/code",
  off_hint: "Turned off in Settings.",
  waiting_hint: "Starts once Claude Code is installed and signed in.",
  starting_hint: "Connecting to Claude.",
  running_hint: (device: string) => `In the Claude app, open ${device}.`,
  running_hint_without_device: "In the Claude app, open this box.",
  last_stop: "Last stop",
  memory: (size: string) => `Uses ${size} of memory, sessions included.`,
  memory_hint: "Memory this server and its sessions use.",
} as const;

export const SERVER_STATES: Record<ServerState, string> = {
  off: "Off",
  waiting: "Waiting",
  starting: "Starting",
  running: "Running",
  retrying: "Restarting",
};
