import { insertNewlineAndIndent, undo } from "@codemirror/commands";
import { EditorSelection, EditorState, type StateCommand } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import type { ParseProblem, SettingsFileFormat } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { checkFile, diagnosticsFor, editorState, fileOf, markServerProblem } from "./code-editor";

function stateOf(text: string, format: SettingsFileFormat = "json") {
  return editorState({
    format,
    text,
    attributes: { "aria-labelledby": "title", "aria-describedby": "problem" },
    onChange: () => {},
    onProblem: () => {},
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

describe("editorState", () => {
  it("keeps a BOM and CRLF through edits", () => {
    const text = '\uFEFF{\r\n  "a": 1\r\n}\r\n';
    const opened = stateOf(text);
    let state = cursorAt(opened, opened.doc.toString().indexOf("1") + 1);
    state = run(typed(state, ","), insertNewlineAndIndent);
    state = typed(state, '"b": 2');
    expect(fileOf(state)).toBe('\uFEFF{\r\n  "a": 1,\r\n  "b": 2\r\n}\r\n');
    expect(fileOf(run(cursorAt(opened, 0), insertNewlineAndIndent))).toBe(
      '\uFEFF\r\n{\r\n  "a": 1\r\n}\r\n',
    );
    const toml = '\uFEFF# mine\r\nmodel = "gpt"\t# odd  spacing\r\n';
    const end = stateOf(toml, "toml").doc.length;
    const added = typed(run(cursorAt(stateOf(toml, "toml"), end), insertNewlineAndIndent), "x = 1");
    expect(fileOf(added)).toBe(`${toml}\r\nx = 1`);
  });

  it("indents after a BOM as if it were not there", () => {
    const tabs = stateOf('\uFEFF{\n\t"a": 1,\n}\n');
    const afterComma = run(
      cursorAt(tabs, tabs.doc.toString().indexOf(",") + 1),
      insertNewlineAndIndent,
    );
    expect(fileOf(afterComma)).toBe('\uFEFF{\n\t"a": 1,\n\t\n}\n');
    expect(fileOf(run(cursorAt(stateOf("\uFEFF{}"), 1), insertNewlineAndIndent))).toBe(
      "\uFEFF{\n  \n}",
    );
    const toml = '\uFEFF# mine\r\nmodel = "o3"\r\n';
    const firstLine = run(cursorAt(stateOf(toml, "toml"), "# mine".length), insertNewlineAndIndent);
    expect(fileOf(typed(firstLine, "x = 1"))).toBe('\uFEFF# mine\r\nx = 1\r\nmodel = "o3"\r\n');
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
    expect(fileOf(opened)).toBe(text);
    const edited = run(typed(cursorAt(opened, opened.doc.length), "x"), insertNewlineAndIndent);
    expect(fileOf(edited).startsWith(`${text}x`)).toBe(true);
    expect(fileOf(run(run(edited, undo), undo))).toBe(text);
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

  it("checks the whole file and reports where it stops the way the server does", () => {
    const reported: (ParseProblem | undefined)[] = [];
    const check = checkFile("json", (problem) => reported.push(problem));
    expect(check({ state: stateOf('\uFEFF{"a":1,}') })).toEqual([
      expect.objectContaining({ from: 7, message: "Trailing comma" }),
    ]);
    expect(check({ state: stateOf("\uFEFF \n") })).toEqual([]);
    expect(reported).toEqual([{ line: 1, column: 9, error: "trailing comma" }, undefined]);
  });

  it("keeps the server's mark until the text changes", () => {
    const check = checkFile("toml", () => {});
    const opened = stateOf("\uFEFFd = 1979-02-30", "toml");
    expect(check({ state: opened })).toEqual([]);
    const marked = markServerProblem(opened, {
      line: 1,
      column: 6,
      error: "invalid date",
    }).state;
    expect(marked.selection.main.head).toBe(4);
    expect(check({ state: marked })).toEqual([
      expect.objectContaining({ from: 4, message: "Invalid date" }),
    ]);
    expect(check({ state: typed(marked, " ") })).toEqual([]);
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
