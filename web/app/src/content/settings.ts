import type { CodexApprovals, CodexSandbox, ReleaseChannel } from "@ezra/client";

export const SETTINGS_DESCRIPTIONS = {
  release_channel: "Release channel",
  release_channel_hint:
    "Switching to stable keeps the installed version until stable has a newer one.",
  remote_control: "Remote Control",
  remote_control_enabled: "Serve to the Claude app",
  remote_control_enabled_hint:
    "Serves ~/projects, and the folders you choose, while Claude Code is signed in.",
  serve_repositories: "Serve new repositories",
  serve_repositories_hint: "Repositories added to ~/projects start with their switch on.",
  spawn: "New sessions in repositories work in",
  spawn_hint: "A folder's Claude Code options can choose otherwise.",
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
  codex_remote_control: "Remote control",
  codex_remote_enabled: "Serve to the ChatGPT app",
  codex_remote_enabled_hint: "Serves this box while Codex is signed in with ChatGPT.",
  codex_sandbox: "Sandbox",
  codex_sandbox_hint: "Read only and Workspace write need a container that allows user namespaces.",
  codex_sandbox_blocked_hint: "Read only and Workspace write fail in this container.",
  codex_approvals: "Approvals",
  codex_approvals_hint: "Codex's questions go to the ChatGPT app.",
  codex_saved: "Codex settings saved.",
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

export const SANDBOX_MODE_ORDER = [
  "danger-full-access",
  "workspace-write",
  "read-only",
] as const satisfies readonly CodexSandbox[];

export const SANDBOX_MODES: Record<CodexSandbox, { title: string; description: string }> = {
  "danger-full-access": {
    title: "No sandbox",
    description: "The container is the only boundary.",
  },
  "workspace-write": {
    title: "Workspace write",
    description:
      "Commands can write in the chat's folder, except .git, and in /tmp, with no network.",
  },
  "read-only": {
    title: "Read only",
    description: "Commands can read files but not change them.",
  },
};

export const APPROVAL_POLICY_ORDER = [
  "on-request",
  "never",
] as const satisfies readonly CodexApprovals[];

export const APPROVAL_POLICIES: Record<CodexApprovals, { title: string; description: string }> = {
  "on-request": {
    title: "On request",
    description: "Codex asks when it needs to, such as before rm\u00a0-\u2060rf.",
  },
  never: {
    title: "Never",
    description: "Codex never asks and refuses commands like rm\u00a0-\u2060rf.",
  },
};
