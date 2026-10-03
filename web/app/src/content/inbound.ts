import type { Agent } from "@ezra/client";

export const INBOUND_COPY = {
  title: "GitHub triggers",
  description:
    "Trigger an agent from your own GitHub issue and pull request conversation comments.",
  onlyAddedRepositories: "Only repositories added to Ezra",
  onlyAddedRepositoriesHint:
    "Turn off to accept commands from any repository. Without a local checkout, Codex chats start in your home folder and Claude Code sessions in ~/projects.",
  shortcuts: "Shortcuts",
  shortcutHint:
    "Shortcuts match anywhere in a comment, including quotes and code. Add --new immediately after the command to start a new chat for that discussion. Model and effort change a Codex chat's defaults for subsequent turns and apply to a Claude Code session when it starts. Leave them blank to keep the current defaults.",
  agent: "Agent",
  trigger: "Command",
  model: "Chat model",
  effort: "Chat effort",
  showModels: {
    claude: "Show Claude models",
    codex: "Show Codex models",
  } satisfies Record<Agent, string>,
  showEfforts: {
    claude: "Show Claude effort levels",
    codex: "Show Codex effort levels",
  } satisfies Record<Agent, string>,
  currentDefault: "Keep current default",
  noSuggestions: {
    claude:
      "Claude Code suggestions appear once its ~/projects server is running. You can still type model and effort values.",
    codex:
      "Codex suggestions appear once Codex is running. You can still type model and effort values.",
  } satisfies Record<Agent, string>,
  add: "Add shortcut",
  remove: "Remove shortcut",
  retention: "History retention in days",
  waitingExpiry: "Waiting request expiry in hours",
  waitingExpiryHint: "Defaults to 24 hours. Requests expire while waiting to be sent to the agent.",
  invalidWaitingExpiry: "Waiting expiry must be a whole number of hours from 1 to 4294967295.",
  pollInterval: "Polling interval in seconds",
  pollIntervalHint: "Wait between GitHub scans. Defaults to 30 seconds.",
  invalidPollInterval: "Polling interval must be a whole number of seconds from 1 to 4294967295.",
  retentionHint:
    "Defaults to 90 days. Cleanup removes integration metadata, not native chats or repositories.",
  feedback: "Status feedback",
  feedbackReactions: "Reactions",
  feedbackFooter: "Edit a status footer into my comment",
  feedbackOff: "Off",
  feedbackHint:
    "Reactions use 🚀 for delivery and 😕 when attention is needed. Footers show received, delivered, unconfirmed, or failed status with the chat name. Feedback never posts a new comment.",
  save: "Save",
  saved: "GitHub trigger settings saved",
  duplicate: "Each shortcut command must be unique.",
  commandRequired: "Every shortcut needs a command.",
  invalidRetention: "Retention must be a whole number of days from 0 to 4294967295.",
};
