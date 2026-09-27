export const SETTINGS_FILE_DESCRIPTIONS = {
  trailing_comma: "trailing comma",
  duplicate_key: "duplicate key",
} as const;

/** CodeMirror's own words, keyed by its English originals. */
export const CODE_EDITOR_PHRASES: Record<string, string> = {
  Diagnostics: "Errors",
  "No diagnostics": "No errors",
  close: "Close",
  "Control character": "Hidden character",
};
