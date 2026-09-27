import { insertNewlineAndIndent, redo, undo } from "@codemirror/commands";
import { EditorSelection, EditorState, type StateCommand } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import type { ParseProblem, SettingsFileFormat } from "@ezra/client";
import { describe, expect, it } from "vitest";

import {
  checkFile,
  diagnosticsFor,
  editorState,
  fileOf,
  loadFile,
  markServerVerdict,
  serverProblemIn,
} from "./code-editor";

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

function loaded(state: EditorState, text: string) {
  const load = loadFile(state, text);
  expect(load).toBeDefined();
  return state.update(load ?? {}).state;
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

  it("loads new text as one change undo and redo reverse, with its BOM and line breaks", () => {
    const opened = stateOf('\uFEFF{\r\n\t"a": 1\r\n}\r\n');
    const edited = typed(cursorAt(opened, opened.doc.length), "x");
    const disk = '{\n  "a": 1,\n  "b": 2\n}\n';
    const reloaded = loaded(edited, disk);
    expect(fileOf(reloaded)).toBe(disk);
    expect(fileOf(run(cursorAt(reloaded, 1), insertNewlineAndIndent))).toBe(
      `{\n  ${disk.slice(1)}`,
    );
    const undone = run(reloaded, undo);
    expect(fileOf(undone)).toBe('\uFEFF{\r\n\t"a": 1\r\n}\r\nx');
    expect(undone.selection.main.head).toBe(edited.selection.main.head);
    expect(fileOf(run(cursorAt(undone, 1), insertNewlineAndIndent))).toBe(
      '\uFEFF{\r\n\t\r\n\t"a": 1\r\n}\r\nx',
    );
    expect(fileOf(run(undone, redo))).toBe(disk);
    expect(fileOf(run(run(run(undone, redo), undo), undo))).toBe('\uFEFF{\r\n\t"a": 1\r\n}\r\n');
  });

  it("changes only what differs, so the cursor stays with its text", () => {
    const text = '{\n  "a": 1,\n  "z": 26\n}\n';
    const opened = cursorAt(stateOf(text), text.indexOf("26"));
    const added = loaded(opened, '{\n  "new": true,\n  "a": 1,\n  "z": 26\n}\n');
    expect(added.sliceDoc(added.selection.main.head, added.selection.main.head + 2)).toBe("26");
    const emoji = stateOf('{"a": "\u{1F600}"}');
    expect(fileOf(loaded(emoji, '{"a": "\u{1F601}"}'))).toBe('{"a": "\u{1F601}"}');
    expect(loadFile(opened, text)).toBeUndefined();
  });

  it("loads a file that differs only in its BOM or line breaks", () => {
    const opened = stateOf("\uFEFF{\r\n}\r\n");
    const reshaped = opened.update(loadFile(opened, "{\n}\n") ?? {});
    expect(reshaped.docChanged).toBe(false);
    expect(reshaped.reconfigured).toBe(true);
    expect(fileOf(reshaped.state)).toBe("{\n}\n");
    expect(fileOf(run(reshaped.state, undo))).toBe("\uFEFF{\r\n}\r\n");
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

  it("checks the whole file and reports where it stops at the column the editor shows", () => {
    const reported: (ParseProblem | undefined)[] = [];
    const check = checkFile("json", (problem) => reported.push(problem));
    expect(check({ state: stateOf('\uFEFF{"a":1,}') })).toEqual([
      expect.objectContaining({ from: 7, message: "Trailing comma" }),
    ]);
    expect(check({ state: stateOf("\uFEFF \n") })).toEqual([]);
    expect(reported).toEqual([{ line: 1, column: 8, error: "trailing comma" }, undefined]);
  });

  it("keeps the server's mark until the text changes", () => {
    const check = checkFile("toml", () => {});
    const opened = stateOf("\uFEFFd = 1979-02-30", "toml");
    expect(check({ state: opened })).toEqual([]);
    const marked = markServerVerdict(opened, {
      line: 1,
      column: 6,
      error: "invalid date",
    }).state;
    expect(marked.selection.main.head).toBe(4);
    expect(serverProblemIn(marked)).toEqual({ line: 1, column: 5, error: "invalid date" });
    expect(check({ state: marked })).toEqual([
      expect.objectContaining({ from: 4, message: "Invalid date" }),
    ]);
    expect(check({ state: typed(marked, " ") })).toEqual([]);
    expect(serverProblemIn(typed(marked, " "))).toBeUndefined();
  });

  it("drops the browser's mark on text the server parsed, until the text changes", () => {
    const reported: (ParseProblem | undefined)[] = [];
    const check = checkFile("toml", (problem) => reported.push(problem));
    const opened = stateOf("t = 07:32:60\n", "toml");
    expect(check({ state: opened })).toEqual([
      expect.objectContaining({ from: 4, message: "Invalid date" }),
    ]);
    const accepted = markServerVerdict(opened).state;
    expect(check({ state: accepted })).toEqual([]);
    expect(check({ state: typed(accepted, "  ") })).toEqual([
      expect.objectContaining({ from: 6, message: "Invalid date" }),
    ]);
    expect(reported).toEqual([
      { line: 1, column: 5, error: "invalid date" },
      { line: 1, column: 7, error: "invalid date" },
    ]);
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
