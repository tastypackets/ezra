import type { Agent, ParseProblem } from "@ezra/client";

export const SETTINGS_FILE_DESCRIPTIONS = {
  trailing_comma: "trailing comma",
  duplicate_key: "duplicate key",
  save: "Save",
  overwrite: "Overwrite",
  revert: "Revert",
  saved: (path: string) => `${path} saved.`,
  changed_on_disk: (path: string) =>
    `${path} changed on disk. Revert loads it, Overwrite replaces it with your text.`,
  problem: ({ line, column, error }: ParseProblem) => `Line ${line}, column ${column}: ${error}`,
  leave_title: "Leave without saving?",
  leave_description: (path: string) => `Leaving discards your changes to ${path}.`,
  stay: "Stay",
  leave: "Discard changes",
} as const;

/** Each agent's own settings file, and what still runs with the old one after a save. */
export const SETTINGS_FILES: Record<Agent, { title: string; still_running: string }> = {
  claude: {
    title: "Claude Code settings.json",
    still_running:
      "Remote Control servers keep the old settings. Restarting them ends their running sessions.",
  },
  codex: {
    title: "Codex config.toml",
    still_running: "Chats already running keep the old settings. Restarting the server ends them.",
  },
};

/** CodeMirror's own words, keyed by its English originals. */
export const CODE_EDITOR_PHRASES: Record<string, string> = {
  Diagnostics: "Errors",
  "No diagnostics": "No errors",
  close: "Close",
  "Control character": "Hidden character",
};
