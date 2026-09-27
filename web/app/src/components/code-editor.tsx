import { setDiagnostics } from "@codemirror/lint";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import type { ParseProblem, SettingsFileFormat } from "@ezra/client";
import { useEffect, useEffectEvent, useRef } from "react";

import { diagnosticsFor, editorExtensions } from "@/lib/code-editor";
import { offsetOf } from "@/lib/settings-text";

interface CodeEditorProps {
  format: SettingsFileFormat;
  /** Read once, remount with a new `key` to load other text. */
  initialText: string;
  /** Where the server said the text stops parsing, marked until the next check. */
  serverProblem?: ParseProblem;
  onChange: (text: string) => void;
  /** Called once typing pauses, with where the text stops parsing or `undefined`. */
  onProblem: (problem: ParseProblem | undefined) => void;
  "aria-labelledby": string;
  "aria-describedby": string;
}

/** Edits a settings file's text, keeping its line breaks and marking where it stops parsing. */
export default function CodeEditor({
  format,
  initialText,
  serverProblem,
  onChange,
  onProblem,
  ...aria
}: CodeEditorProps) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>(undefined);
  const initial = useEffectEvent(() => ({ text: initialText, attributes: aria }));
  const changed = useEffectEvent(onChange);
  const checked = useEffectEvent(onProblem);

  useEffect(() => {
    const element = parent.current;
    if (!element) {
      return undefined;
    }
    const { text, attributes } = initial();
    const created = new EditorView({
      parent: element,
      state: EditorState.create({
        doc: text,
        extensions: editorExtensions({
          format,
          text,
          attributes,
          onChange: changed,
          onProblem: checked,
        }),
      }),
    });
    view.current = created;
    return () => {
      view.current = undefined;
      created.destroy();
    };
  }, [format]);

  useEffect(() => {
    const current = view.current;
    if (!current || !serverProblem) {
      return;
    }
    const at = offsetOf(current.state.doc, serverProblem);
    current.dispatch(
      setDiagnostics(current.state, diagnosticsFor({ at, error: serverProblem.error })),
    );
  }, [serverProblem]);

  return (
    <div
      ref={parent}
      className="overflow-hidden rounded-lg border border-input font-mono text-base transition-colors focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 has-aria-invalid:border-destructive has-aria-invalid:ring-3 has-aria-invalid:ring-destructive/20 md:text-sm dark:bg-input/30 dark:has-aria-invalid:border-destructive/50 dark:has-aria-invalid:ring-destructive/40"
    />
  );
}
