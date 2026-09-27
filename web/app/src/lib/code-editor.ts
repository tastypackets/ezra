import {
  defaultKeymap,
  history,
  historyKeymap,
  invertedEffects,
  isolateHistory,
} from "@codemirror/commands";
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
  Compartment,
  EditorState,
  type Extension,
  Facet,
  StateEffect,
  StateField,
  Text,
  type Transaction,
  type TransactionSpec,
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

/** What the file keeps around the text in the editor: its leading BOM, line break and indent. */
interface FileShape {
  prefix: string;
  lineBreak: string;
  indent: string;
}

const SHAPE = new Compartment();

const reshaped = StateEffect.define();

function shapeOf(text: string): FileShape {
  return {
    prefix: text.startsWith(BOM) ? BOM : "",
    lineBreak: lineBreakOf(text),
    indent: /^\t/m.test(text) ? "\t" : "  ",
  };
}

function shapeIn(state: EditorState): FileShape {
  return {
    prefix: state.facet(KEPT_PREFIX),
    lineBreak: state.lineBreak,
    indent: state.facet(indentUnit),
  };
}

function shapeExtension({ prefix, lineBreak, indent }: FileShape): Extension {
  return [KEPT_PREFIX.of(prefix), EditorState.lineSeparator.of(lineBreak), indentUnit.of(indent)];
}

function reshape(shape: FileShape): StateEffect<unknown>[] {
  return [reshaped.of(null), SHAPE.reconfigure(shapeExtension(shape))];
}

/** True when `transaction` gave the file another BOM, line break or indent. */
function reshapes(transaction: Transaction): boolean {
  return transaction.effects.some((effect) => effect.is(reshaped));
}

/** How many UTF-16 units `before` and `after` share at their start, not splitting a character. */
function sharedStart(before: string, after: string): number {
  const shorter = Math.min(before.length, after.length);
  let shared = 0;
  while (shared < shorter && before.charCodeAt(shared) === after.charCodeAt(shared)) {
    shared += 1;
  }
  return shared > 0 && /[\uD800-\uDBFF]/.test(before.charAt(shared - 1)) ? shared - 1 : shared;
}

/** How many UTF-16 units `before` and `after` share at their end, not splitting a character or overlapping `start`. */
function sharedEnd(before: string, after: string, start: number): number {
  const shorter = Math.min(before.length, after.length) - start;
  let shared = 0;
  while (
    shared < shorter &&
    before.charCodeAt(before.length - 1 - shared) === after.charCodeAt(after.length - 1 - shared)
  ) {
    shared += 1;
  }
  return shared > 0 && /[\uDC00-\uDFFF]/.test(before.charAt(before.length - shared))
    ? shared - 1
    : shared;
}

/** Replaces the file in the editor with `text` as one undoable change to what differs, `undefined` when nothing does. */
export function loadFile(state: EditorState, text: string): TransactionSpec | undefined {
  const shape = shapeOf(text);
  const current = shapeIn(state);
  const sameShape =
    shape.prefix === current.prefix &&
    shape.lineBreak === current.lineBreak &&
    shape.indent === current.indent;
  const doc = Text.of(text.slice(shape.prefix.length).split(shape.lineBreak));
  const before = state.doc.toString();
  const after = doc.toString();
  const start = sharedStart(before, after);
  const end = sharedEnd(before, after, start);
  if (sameShape && start === before.length && start === after.length) {
    return undefined;
  }
  return {
    changes: { from: start, to: before.length - end, insert: doc.slice(start, after.length - end) },
    effects: sameShape ? [] : reshape(shape),
    annotations: isolateHistory.of("full"),
  };
}

const setServerVerdict = StateEffect.define<TextProblem | null>();

/** Where the server said the text in the editor stops parsing, `null` when it parsed, until the text changes. */
const SERVER_VERDICT = StateField.define<TextProblem | null | undefined>({
  create: () => undefined,
  update: (verdict, transaction) => {
    for (const effect of transaction.effects) {
      if (effect.is(setServerVerdict)) {
        return effect.value;
      }
    }
    return transaction.docChanged || reshapes(transaction) ? undefined : verdict;
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

/** Checks the file as `format`, leaving the server's verdict in place while the text is as sent. */
export function checkFile(
  format: SettingsFileFormat,
  onProblem: (problem: ParseProblem | undefined) => void,
) {
  return ({ state }: { state: EditorState }): Diagnostic[] => {
    const verdict = state.field(SERVER_VERDICT);
    if (verdict !== undefined) {
      return diagnosticsFor(verdict ?? undefined);
    }
    const file = state.toText(fileOf(state));
    const problem = FIND_PROBLEM[format](file);
    onProblem(problem && parseProblemAt(file, problem));
    return diagnosticsFor(problem && inEditor(state, problem));
  };
}

/** Marks where the server said the text stops parsing and moves the cursor there, or clears the mark when it parsed. */
export function markServerVerdict(state: EditorState, problem?: ParseProblem): Transaction {
  if (!problem) {
    return state.update(setDiagnostics(state, []), { effects: setServerVerdict.of(null) });
  }
  const marked = inEditor(state, {
    at: offsetOf(state.toText(fileOf(state)), problem),
    error: problem.error,
  });
  return state.update(setDiagnostics(state, diagnosticsFor(marked)), {
    effects: setServerVerdict.of(marked),
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
  const shape = shapeOf(text);
  return EditorState.create({
    doc: text.slice(shape.prefix.length),
    extensions: [
      SETUP,
      LANGUAGES[format],
      SHAPE.of(shapeExtension(shape)),
      invertedEffects.of((transaction) =>
        reshapes(transaction) ? reshape(shapeIn(transaction.startState)) : [],
      ),
      SERVER_VERDICT,
      EditorView.clipboardInputFilter.of((input, state) =>
        input.replace(/\r\n?|\n/g, state.lineBreak),
      ),
      linter(checkFile(format, onProblem), {
        delay: CHECK_DELAY_MS,
        needsRefresh: (update) => update.transactions.some(reshapes),
      }),
      EditorView.contentAttributes.of(attributes),
      EditorView.contentAttributes.of(({ state }) =>
        diagnosticCount(state) ? { "aria-invalid": "true" } : null,
      ),
      EditorView.updateListener.of((update) => {
        if (update.docChanged || update.transactions.some(reshapes)) {
          onChange(fileOf(update.state));
        }
      }),
    ],
  });
}
