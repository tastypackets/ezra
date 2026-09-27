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
import {
  type Diagnostic,
  diagnosticCount,
  linter,
  lintGutter,
  lintKeymap,
  setDiagnostics,
} from "@codemirror/lint";
import {
  EditorState,
  type Extension,
  Facet,
  StateEffect,
  StateField,
  type Transaction,
} from "@codemirror/state";
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
  BOM,
  FIND_PROBLEM,
  lineBreakOf,
  offsetOf,
  parseProblemAt,
  type TextProblem,
} from "@/lib/settings-text";
import { capitalized } from "@/lib/utils";

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

/** The file's leading BOM, which the text in the editor leaves out. */
const KEPT_PREFIX = Facet.define<string, string>({ combine: (prefixes) => prefixes[0] ?? "" });

const setServerProblem = StateEffect.define<TextProblem>();

/** Where the server said the text in the editor stops parsing, until the text changes. */
const SERVER_PROBLEM = StateField.define<TextProblem | undefined>({
  create: () => undefined,
  update: (problem, transaction) => {
    for (const effect of transaction.effects) {
      if (effect.is(setServerProblem)) {
        return effect.value;
      }
    }
    return transaction.docChanged ? undefined : problem;
  },
});

/** The whole file the editor holds, with the BOM it keeps out of the editor. */
export function fileOf(state: EditorState): string {
  return state.facet(KEPT_PREFIX) + state.sliceDoc();
}

/** `problem`, found in the whole file, as an offset into the editor. */
function inEditor(state: EditorState, problem: TextProblem): TextProblem {
  return { ...problem, at: Math.max(problem.at - state.facet(KEPT_PREFIX).length, 0) };
}

/** An error mark at `problem`, none when there is no problem. */
export function diagnosticsFor(problem: TextProblem | undefined): Diagnostic[] {
  return problem
    ? [{ from: problem.at, to: problem.at, severity: "error", message: capitalized(problem.error) }]
    : [];
}

/** Checks the file as `format`, keeping the server's mark instead while the text is as sent. */
export function checkFile(
  format: SettingsFileFormat,
  onProblem: (problem: ParseProblem | undefined) => void,
) {
  return ({ state }: { state: EditorState }): Diagnostic[] => {
    const file = state.toText(fileOf(state));
    const problem = FIND_PROBLEM[format](file);
    onProblem(problem && parseProblemAt(file, problem));
    return diagnosticsFor(state.field(SERVER_PROBLEM) ?? (problem && inEditor(state, problem)));
  };
}

/** Marks where the server said the text stops parsing and moves the cursor there. */
export function markServerProblem(state: EditorState, problem: ParseProblem): Transaction {
  const marked = inEditor(state, {
    at: offsetOf(state.toText(fileOf(state)), problem),
    error: problem.error,
  });
  return state.update(setDiagnostics(state, diagnosticsFor(marked)), {
    effects: setServerProblem.of(marked),
    selection: { anchor: marked.at },
    scrollIntoView: true,
  });
}

/** An editor for the file `text` as `format`, reporting each edit as the whole file. */
export function editorState({
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
}): EditorState {
  const prefix = text.startsWith(BOM) ? BOM : "";
  return EditorState.create({
    doc: text.slice(prefix.length),
    extensions: [
      SETUP,
      LANGUAGES[format],
      KEPT_PREFIX.of(prefix),
      SERVER_PROBLEM,
      EditorState.lineSeparator.of(lineBreakOf(text)),
      indentUnit.of(/^\t/m.test(text) ? "\t" : "  "),
      EditorView.clipboardInputFilter.of((input, state) =>
        input.replace(/\r\n?|\n/g, state.lineBreak),
      ),
      linter(checkFile(format, onProblem), { delay: CHECK_DELAY_MS }),
      EditorView.contentAttributes.of(attributes),
      EditorView.contentAttributes.of(({ state }) =>
        diagnosticCount(state) ? { "aria-invalid": "true" } : null,
      ),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) {
          onChange(fileOf(update.state));
        }
      }),
    ],
  });
}
