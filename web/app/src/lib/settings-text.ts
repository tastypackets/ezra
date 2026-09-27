import { jsonLanguage } from "@codemirror/lang-json";
import type { Text } from "@codemirror/state";
import type { ParseProblem, SettingsFileFormat } from "@ezra/client";
import { parse as parseToml, TomlError } from "smol-toml";

import { SETTINGS_FILE_DESCRIPTIONS } from "@/content/settings-file";

/** Where a document stops parsing, as an offset into it. */
export interface TextProblem {
  at: number;
  error: string;
}

/** The byte order mark, U+FEFF. */
export const BOM = "\uFEFF";

/** The line break to keep for `text`: `\r\n` only when every line break in it is one. */
export function lineBreakOf(text: string): "\n" | "\r\n" {
  return text.includes("\r\n") && !/\r(?!\n)|(?<!\r)\n/.test(text) ? "\r\n" : "\n";
}

/** The offset of a one-based line and UTF-16 column, counting `\r\n`, `\r` and `\n` as breaks. */
function offsetInText(text: string, line: number, column: number): number {
  const breaks = /\r\n?|\n/g;
  let start = 0;
  for (let seen = 1; seen < line && breaks.exec(text); seen += 1) {
    start = breaks.lastIndex;
  }
  return start + column - 1;
}

const LITERALS = ["true", "false", "null"];

/** Where a misspelled `true`, `false` or `null` starting at `at` goes wrong. */
function misspeltAt(json: string, at: number): number {
  const literal = LITERALS.find((word) => word[0] === json[at]);
  const wrong = literal
    ? Array.from(literal).findIndex((letter, index) => json[at + index] !== letter)
    : -1;
  return wrong > 0 ? at + wrong : at;
}

/** Where `json` stops parsing and why, from a `JSON.parse` error. */
export function jsonErrorIn(json: string, message: string): TextProblem {
  const position = /JSON at position (\d+)/.exec(message);
  const lineColumn = /at line (\d+) column (\d+) of the JSON data$/.exec(message);
  let at: number | undefined;
  if (position) {
    at = Number(position[1]);
  } else if (lineColumn) {
    at = offsetInText(json, Number(lineColumn[1]), Number(lineColumn[2]));
    if (message.includes("unexpected keyword")) {
      at = misspeltAt(json, at);
    }
  } else if (/end of JSON input|EOF/.test(message)) {
    at = json.length;
  } else {
    jsonLanguage.parser.parse(json).iterate({
      enter: (node) => {
        if (node.type.isError) {
          at ??= node.from;
        }
        return at === undefined;
      },
    });
    at = misspeltAt(json, at ?? json.length);
  }
  at = Math.min(Math.max(at ?? json.length, 0), json.length);
  if (at === json.length && at > 0) {
    at -= (json.codePointAt(at - 2) ?? 0) > 0xffff ? 2 : 1;
  }
  return {
    at,
    error: message
      .replace(/^JSON(\.parse:| Parse error:) /, "")
      .replace(/( in JSON)? at position \d+[\s\S]*$/, "")
      .replace(/ at line \d+ column \d+ of the JSON data$/, "")
      .replace(/, (\.\.\.)?"[\s\S]*"(\.\.\.)? is not valid JSON$/, ""),
  };
}

/** Where `doc` stops parsing the way Claude Code reads settings.json. */
function jsonProblem(doc: Text): TextProblem | undefined {
  const text = doc.toString();
  if (text.trim() === "") {
    return undefined;
  }
  const skipped = text.startsWith(BOM) ? BOM.length : 0;
  const json = text.slice(skipped);
  try {
    JSON.parse(json);
    return undefined;
  } catch (error) {
    const found = jsonErrorIn(json, error instanceof Error ? error.message : String(error));
    const trailingComma =
      /[\]}]/.test(json.charAt(found.at)) && json.slice(0, found.at).trimEnd().endsWith(",");
    return {
      at: found.at + skipped,
      error: trailingComma ? SETTINGS_FILE_DESCRIPTIONS.trailing_comma : found.error,
    };
  }
}

/** Where `doc` stops parsing as TOML 1.1. */
function tomlProblem(doc: Text): TextProblem | undefined {
  try {
    parseToml(doc.toString(), { integersAsBigInt: true });
    return undefined;
  } catch (error) {
    if (!(error instanceof TomlError)) {
      throw error;
    }
    const line = doc.line(Math.min(Math.max(error.line, 1), doc.lines));
    return {
      at: Math.min(line.from + error.column - 1, line.to),
      error: error.message.startsWith("Invalid TOML document: trying to redefine")
        ? SETTINGS_FILE_DESCRIPTIONS.duplicate_key
        : (error.message.split("\n")[0] ?? error.message).replace(/^Invalid TOML document: /, ""),
    };
  }
}

/** Where a document stops parsing, for each format, `undefined` when it parses. */
export const FIND_PROBLEM: Record<SettingsFileFormat, (doc: Text) => TextProblem | undefined> = {
  json: jsonProblem,
  toml: tomlProblem,
};

/** Whether a problem found in the browser blocks Save, only where its check is the agent's own parser. */
export const BROWSER_CHECK_BLOCKS_SAVE: Record<SettingsFileFormat, boolean> = {
  json: true,
  toml: false,
};

/** `problem` with a one-based line and a column in characters, as the server reports it. */
export function parseProblemAt(doc: Text, problem: TextProblem): ParseProblem {
  const line = doc.lineAt(problem.at);
  return {
    error: problem.error,
    line: line.number,
    column: Array.from(doc.sliceString(line.from, problem.at)).length + 1,
  };
}

/** The offset of the server's one-based line and character column in `doc`. */
export function offsetOf(doc: Text, { line, column }: Pick<ParseProblem, "line" | "column">) {
  const found = doc.line(Math.min(Math.max(line, 1), doc.lines));
  return (
    found.from +
    Array.from(found.text)
      .slice(0, Math.max(column - 1, 0))
      .join("").length
  );
}
