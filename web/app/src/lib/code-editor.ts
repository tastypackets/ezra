import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { json } from "@codemirror/lang-json";
import {
  bracketMatching,
  HighlightStyle,
  indentOnInput,
  indentUnit,
  StreamLanguage,
  syntaxHighlighting,
} from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { type Diagnostic, diagnosticCount, linter, lintGutter, lintKeymap } from "@codemirror/lint";
import { EditorState, type Extension } from "@codemirror/state";
import {
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import type { ParseProblem, SettingsFileFormat } from "@ezra/client";
import { tags } from "@lezer/highlight";

import { CODE_EDITOR_PHRASES } from "@/content/settings-file";
import {
  capitalized,
  FIND_PROBLEM,
  lineBreakOf,
  parseProblemAt,
  type TextProblem,
} from "@/lib/settings-text";

/** How long typing pauses before the text is checked, in milliseconds. */
const CHECK_DELAY_MS = 300;

const THEME = EditorView.theme({
  "&": { color: "var(--foreground)", backgroundColor: "transparent", maxHeight: "32rem" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "inherit", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--foreground)" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "var(--foreground)" },
  ".cm-gutters": {
    backgroundColor: "transparent",
    color: "var(--muted-foreground)",
    borderRight: "1px solid var(--border)",
  },
  ".cm-activeLine, .cm-activeLineGutter": {
    backgroundColor: "color-mix(in oklch, var(--muted) 60%, transparent)",
  },
  ".cm-selectionBackground, &.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-content ::selection":
    { backgroundColor: "color-mix(in oklch, var(--ring) 35%, transparent)" },
  "&.cm-focused .cm-matchingBracket": {
    backgroundColor: "color-mix(in oklch, var(--ring) 30%, transparent)",
  },
  ".cm-line .cm-specialChar": { color: "var(--muted-foreground)" },
  ".cm-tooltip": {
    backgroundColor: "var(--popover)",
    color: "var(--popover-foreground)",
    border: "1px solid var(--border)",
    borderRadius: "calc(var(--radius) * 0.8)",
  },
  ".cm-panels": { backgroundColor: "var(--card)", color: "var(--card-foreground)" },
  ".cm-panels-bottom": { borderTop: "1px solid var(--border)" },
  ".cm-panel.cm-panel-lint ul [aria-selected], .cm-panel.cm-panel-lint ul:focus [aria-selected]": {
    backgroundColor: "var(--accent)",
    color: "var(--accent-foreground)",
  },
  ".cm-diagnostic-error": { borderLeftColor: "var(--destructive)" },
  ".cm-lintPoint-error:after": { borderBottomColor: "var(--destructive)" },
});

const HIGHLIGHT = syntaxHighlighting(
  HighlightStyle.define([
    { tag: tags.propertyName, color: "var(--syntax-key)" },
    { tag: tags.string, color: "var(--syntax-string)" },
    { tag: [tags.number, tags.bool, tags.null, tags.atom], color: "var(--syntax-literal)" },
    { tag: [tags.comment, tags.punctuation, tags.bracket], color: "var(--muted-foreground)" },
  ]),
);

const SETUP: Extension = [
  lineNumbers(),
  highlightActiveLineGutter(),
  highlightSpecialChars({ addSpecialChars: /\u00a0/ }),
  highlightActiveLine(),
  history(),
  drawSelection(),
  indentOnInput(),
  bracketMatching(),
  lintGutter(),
  keymap.of([...defaultKeymap, ...historyKeymap, ...lintKeymap]),
  EditorView.lineWrapping,
  EditorState.phrases.of(CODE_EDITOR_PHRASES),
  THEME,
  HIGHLIGHT,
];

const LANGUAGES: Record<SettingsFileFormat, Extension> = {
  json: json(),
  toml: StreamLanguage.define(toml),
};

/** An error mark at `problem`, none when there is no problem. */
export function diagnosticsFor(problem: TextProblem | undefined): Diagnostic[] {
  return problem
    ? [{ from: problem.at, to: problem.at, severity: "error", message: capitalized(problem.error) }]
    : [];
}

/** Everything the editor does with `text` in `format`, reporting edits and parse checks. */
export function editorExtensions({
  format,
  text,
  attributes,
  onChange,
  onProblem,
}: {
  format: SettingsFileFormat;
  text: string;
  attributes: Record<string, string>;
  onChange: (text: string) => void;
  onProblem: (problem: ParseProblem | undefined) => void;
}): Extension {
  return [
    SETUP,
    LANGUAGES[format],
    EditorState.lineSeparator.of(lineBreakOf(text)),
    indentUnit.of(/^\t/m.test(text) ? "\t" : "  "),
    EditorView.clipboardInputFilter.of((input, state) =>
      input.replace(/\r\n?|\n/g, state.lineBreak),
    ),
    linter(
      ({ state }) => {
        const problem = FIND_PROBLEM[format](state.doc);
        onProblem(problem && parseProblemAt(state.doc, problem));
        return diagnosticsFor(problem);
      },
      { delay: CHECK_DELAY_MS },
    ),
    EditorView.contentAttributes.of(attributes),
    EditorView.contentAttributes.of(({ state }) =>
      diagnosticCount(state) ? { "aria-invalid": "true" } : null,
    ),
    EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        onChange(update.state.sliceDoc());
      }
    }),
  ];
}
