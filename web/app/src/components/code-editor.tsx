import { EditorView } from "@codemirror/view";
import type { ParseProblem, SettingsFileFormat } from "@ezra/client";
import { type Ref, useEffect, useEffectEvent, useImperativeHandle, useRef } from "react";

import { editorState, loadFile, markServerVerdict, serverProblemIn } from "@/lib/code-editor";

/** What a parent can do with a mounted editor. */
export interface CodeEditorHandle {
  focus: () => void;
}

interface CodeEditorProps {
  ref?: Ref<CodeEditorHandle>;
  format: SettingsFileFormat;
  /** The file to show. Each new object loads its text as one change undo can reverse, keeping the cursor and scroll where the text is unchanged. */
  loaded: { text: string };
  /** What the server said about the text, kept until the text changes: where it stops parsing, with the cursor moved there, or no problem. */
  serverVerdict?: { problem?: ParseProblem };
  onChange: (text: string) => void;
  /** Called with where the editor marks the text as not parsing, at the line and column it shows, or `undefined`: once typing pauses, and on each verdict from the server. */
  onProblem: (problem: ParseProblem | undefined) => void;
  "aria-labelledby": string;
  "aria-describedby": string;
}

/** Edits a settings file's text, keeping its line breaks and marking where it stops parsing. */
export default function CodeEditor({
  ref,
  format,
  loaded,
  serverVerdict,
  onChange,
  onProblem,
  ...aria
}: CodeEditorProps) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView>(undefined);
  const initial = useEffectEvent(() => ({ text: loaded.text, attributes: aria }));
  const changed = useEffectEvent(onChange);
  const checked = useEffectEvent(onProblem);

  useImperativeHandle(ref, () => ({ focus: () => view.current?.focus() }), []);

  useEffect(() => {
    const element = parent.current;
    if (!element) {
      return undefined;
    }
    const { text, attributes } = initial();
    const created = new EditorView({
      parent: element,
      state: editorState({ format, text, attributes, onChange: changed, onProblem: checked }),
    });
    view.current = created;
    return () => {
      view.current = undefined;
      created.destroy();
    };
  }, [format]);

  useEffect(() => {
    const current = view.current;
    const load = current && loadFile(current.state, loaded.text);
    if (current && load) {
      current.dispatch(load);
    }
  }, [loaded]);

  useEffect(() => {
    const current = view.current;
    if (current && serverVerdict) {
      current.dispatch(markServerVerdict(current.state, serverVerdict.problem));
      checked(serverProblemIn(current.state));
    }
  }, [serverVerdict]);

  return (
    <div
      ref={parent}
      className="overflow-hidden rounded-lg border border-input font-mono text-base transition-colors focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 has-aria-invalid:border-destructive has-aria-invalid:ring-3 has-aria-invalid:ring-destructive/20 md:text-sm dark:bg-input/30 dark:has-aria-invalid:border-destructive/50 dark:has-aria-invalid:ring-destructive/40 md:[&_.cm-editor]:max-h-128 max-md:[&_.cm-gutter-lint]:hidden!"
    />
  );
}
