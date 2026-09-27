import { insertNewlineAndIndent, undo } from "@codemirror/commands";
import { EditorSelection, EditorState, type StateCommand } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import type { SettingsFileFormat } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { diagnosticsFor, editorExtensions } from "./code-editor";

function stateOf(text: string, format: SettingsFileFormat = "json") {
  return EditorState.create({
    doc: text,
    extensions: editorExtensions({
      format,
      text,
      attributes: { "aria-labelledby": "title", "aria-describedby": "problem" },
      onChange: () => {},
      onProblem: () => {},
    }),
  });
}

function run(state: EditorState, command: StateCommand) {
  let next = state;
  command({
    state,
    dispatch: (transaction) => {
      next = transaction.state;
    },
  });
  return next;
}

function cursorAt(state: EditorState, at: number) {
  return state.update({ selection: EditorSelection.cursor(at) }).state;
}

function typed(state: EditorState, text: string) {
  return state.update(state.replaceSelection(text)).state;
}

function pasted(state: EditorState, text: string) {
  return state
    .facet(EditorView.clipboardInputFilter)
    .reduce((input, filter) => filter(input, state), text);
}

describe("editorExtensions", () => {
  it("keeps a BOM and CRLF through edits", () => {
    const text = '\uFEFF{\r\n  "a": 1\r\n}\r\n';
    const opened = stateOf(text);
    let state = cursorAt(opened, opened.doc.toString().indexOf("1") + 1);
    state = run(typed(state, ","), insertNewlineAndIndent);
    state = typed(state, '"b": 2');
    expect(state.sliceDoc()).toMatch(/^\uFEFF\{\r\n {2}"a": 1,\r\n +"b": 2\r\n\}\r\n$/);
    expect(state.update({ changes: { from: 0, to: 1 } }).state.sliceDoc()).toMatch(
      /^\{\r\n {2}"a": 1,\r\n +"b": 2\r\n\}\r\n$/,
    );
    const toml = '\uFEFF# mine\r\nmodel = "gpt"\t# odd  spacing\r\n';
    const end = stateOf(toml, "toml").doc.length;
    const added = typed(run(cursorAt(stateOf(toml, "toml"), end), insertNewlineAndIndent), "x = 1");
    expect(added.sliceDoc()).toBe(`${toml}\r\nx = 1`);
    expect(added.sliceDoc().startsWith("\uFEFF")).toBe(true);
  });

  it.each([
    ["json", '\uFEFF{\r\n\t"a" :   1 ,\r\n  "b":[ ]\r\n}'],
    ["json", '{"env": {"A": "\u00E9"}}\n\n\n'],
    ["json", "\uFEFF"],
    ["json", ""],
    ["json", "{}\n\n\n"],
    ["json", '{\r\n"a":\r1\n}'],
    ["toml", '\uFEFF# mine\r\nmodel = "gpt"   # trailing\r\n\r\n[features]\r\n\tx = true'],
    ["toml", "a = 1"],
  ] as const)("gives back %s %j byte for byte after an edit and undo", (format, text) => {
    const opened = stateOf(text, format);
    expect(opened.sliceDoc()).toBe(text);
    const edited = run(typed(cursorAt(opened, opened.doc.length), "x"), insertNewlineAndIndent);
    expect(edited.sliceDoc().startsWith(`${text}x`)).toBe(true);
    expect(run(run(edited, undo), undo).sliceDoc()).toBe(text);
  });

  it("indents new lines with tabs in a file indented with tabs", () => {
    const toml = "[features]\n\tx = true\n";
    const afterTab = run(cursorAt(stateOf(toml, "toml"), toml.length - 1), insertNewlineAndIndent);
    expect(afterTab.sliceDoc()).toBe("[features]\n\tx = true\n\t\n");
    const json = '{\n\t"a": {\n\t\t"b": 1\n\t}\n}\n';
    const nested = run(cursorAt(stateOf(json), json.indexOf("1") + 1), insertNewlineAndIndent);
    expect(nested.sliceDoc()).toBe('{\n\t"a": {\n\t\t"b": 1\n\t\t\n\t}\n}\n');
    const spaced = run(cursorAt(stateOf('{\n  "a": 1\n}'), 10), insertNewlineAndIndent);
    expect(spaced.sliceDoc()).toBe('{\n  "a": 1\n  \n}');
  });

  it("keeps LF, a lone CR and mixed line breaks through edits", () => {
    const lf = run(cursorAt(stateOf("{}\n"), 1), insertNewlineAndIndent);
    expect(lf.sliceDoc()).toBe("{\n  \n}\n");
    const mixed = '{\r\n"a":\r1\n}';
    const edited = typed(cursorAt(stateOf(mixed), mixed.length - 1), "  ");
    expect(edited.sliceDoc()).toBe('{\r\n"a":\r1\n  }');
    expect(run(cursorAt(edited, 2), insertNewlineAndIndent).sliceDoc()).toMatch(
      /^\{\r\n *\n"a":\r1\n {2}\}$/,
    );
  });

  it("gives pasted text the file's line break", () => {
    expect(pasted(stateOf("{\r\n}"), "a\r\nb\nc\rd")).toBe("a\r\nb\r\nc\r\nd");
    expect(pasted(stateOf("{\n}"), "a\r\nb\nc\rd")).toBe("a\nb\nc\nd");
    expect(pasted(stateOf("a = 1\r\n", "toml"), "b = 2\n")).toBe("b = 2\r\n");
  });

  it("leaves Tab to move focus and binds the error list", () => {
    const keys = stateOf("{}")
      .facet(keymap)
      .flat()
      .map((binding) => binding.key);
    expect(keys).not.toContain("Tab");
    expect(keys).not.toContain("Shift-Tab");
    expect(keys).toEqual(expect.arrayContaining(["Mod-Shift-m", "F8"]));
  });

  it("names the error list and hidden characters in ezra's words", () => {
    const state = stateOf("{}");
    expect(state.phrase("Diagnostics")).toBe("Errors");
    expect(state.phrase("No diagnostics")).toBe("No errors");
    expect(state.phrase("close")).toBe("Close");
    expect(state.phrase("Control character")).toBe("Hidden character");
  });

  it("starts error messages with a capital letter", () => {
    expect(diagnosticsFor({ at: 0, error: "trailing comma" })).toEqual([
      expect.objectContaining({ message: "Trailing comma" }),
    ]);
    expect(diagnosticsFor(undefined)).toEqual([]);
  });

  it("labels the text", () => {
    const attributes = stateOf("{}")
      .facet(EditorView.contentAttributes)
      .filter((value) => typeof value !== "function")
      .reduce<Record<string, string>>((all, value) => ({ ...all, ...value }), {});
    expect(attributes).toMatchObject({
      "aria-labelledby": "title",
      "aria-describedby": "problem",
    });
  });
});
