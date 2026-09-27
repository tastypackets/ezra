import { Text } from "@codemirror/state";
import type { SettingsFileFormat } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { FIND_PROBLEM, jsonErrorIn, lineBreakOf, offsetOf, parseProblemAt } from "./settings-text";

function docOf(text: string) {
  return Text.of(text.split(lineBreakOf(text)));
}

function problem(format: SettingsFileFormat, text: string) {
  const doc = docOf(text);
  const found = FIND_PROBLEM[format](doc);
  return found && parseProblemAt(doc, found);
}

function at(line: number, column: number) {
  return expect.objectContaining({ line, column });
}

describe("lineBreakOf", () => {
  it("keeps CRLF only when every line break is one", () => {
    expect(lineBreakOf("")).toBe("\n");
    expect(lineBreakOf("{}\n")).toBe("\n");
    expect(lineBreakOf("{\r\n}\r\n")).toBe("\r\n");
    expect(lineBreakOf("\uFEFF{\r\n\t}")).toBe("\r\n");
    expect(lineBreakOf("{\r\n}\n")).toBe("\n");
    expect(lineBreakOf("a\rb\r\n")).toBe("\n");
    expect(lineBreakOf("a\r")).toBe("\n");
  });
});

describe("JSON, the way Claude Code reads settings.json", () => {
  it("accepts what JSON.parse accepts after one BOM, and blank text", () => {
    for (const valid of [
      "",
      " \n\t\r\n",
      "\uFEFF",
      "\uFEFF\u00A0\u2028\u3000 ",
      "\uFEFF{}",
      "{}",
      "[]",
      "1",
      "null",
      '{"unknown":{"keys":[1,"two",null]},"a":"\\ud800","b":1e400}',
      '{"a":"x\u2028y"}',
      '{\r\n\t"a" :  1\r\n}\r\n\r\n',
      "{\r\n}\n\r",
      `${"[".repeat(200)}${"]".repeat(200)}`,
    ]) {
      expect(problem("json", valid), JSON.stringify(valid)).toBeUndefined();
    }
  });

  it("rejects what Claude Code rejects, at the server's line and character column", () => {
    expect(problem("json", '{"a":1,}')).toEqual(at(1, 8));
    expect(problem("json", '{\r\n  "a": 1,\r\n}\r\n')).toEqual(at(3, 1));
    expect(problem("json", '\uFEFF{"a":1,}')).toEqual(at(1, 9));
    expect(problem("json", '{"\u00E9\u00E9\u00E9":1,}')).toEqual(at(1, 10));
    expect(problem("json", '{\n"\u{1F600}": 1,\n"b": x}')).toEqual(at(3, 6));
    expect(problem("json", "// c\n{}")).toEqual(at(1, 1));
    expect(problem("json", "{/* c */}")).toEqual(at(1, 2));
    expect(problem("json", '{"a":01}')).toEqual(at(1, 7));
    expect(problem("json", "{'a':1}")).toEqual(at(1, 2));
    expect(problem("json", "{} x")).toEqual(at(1, 4));
    expect(problem("json", "\uFEFF{} x")).toEqual(at(1, 5));
    expect(problem("json", "{}}")).toEqual(at(1, 3));
    expect(problem("json", '{"a":1}\n{"b":2}\n')).toEqual(at(2, 1));
    expect(problem("json", '{"a":"x\ty"}')).toEqual(at(1, 8));
    expect(problem("json", '{"a":NaN}')).toEqual(at(1, 6));
    expect(problem("json", "\u00A0{}")).toEqual(at(1, 1));
    expect(problem("json", "\uFEFF\uFEFF{}")).toEqual(at(1, 2));
    expect(problem("json", "\u0085")).toEqual(at(1, 1));
    expect(problem("json", '{\n  "model": "opus\n}\n')).toEqual(at(2, 17));
    expect(problem("json", '{\r\n  "model": "opus\r\n}\r\n')).toEqual(at(2, 17));
    expect(problem("json", '{"a":"\\ud800","b":1,}')).toEqual(at(1, 21));
    expect(problem("json", `${"[".repeat(200)}1,${"]".repeat(200)}`)).toEqual(at(1, 203));
  });

  it("points into a misspelled true, false or null where the server does", () => {
    expect(problem("json", '{"a": ture}')).toEqual(at(1, 8));
    expect(problem("json", '{"a": True}')).toEqual(at(1, 7));
    expect(problem("json", '{"a": nul}')).toEqual(at(1, 10));
    expect(problem("json", '{"a":tru}')).toEqual(at(1, 9));
    expect(problem("json", '{"a": faase}')).toEqual(at(1, 9));
    expect(problem("json", '{"a": nan}')).toEqual(at(1, 8));
    expect(problem("json", '{"a": [tru]}')).toEqual(at(1, 11));
    expect(problem("json", '{\n  "a": true,\n  "b": fasle\n}\n')).toEqual(at(3, 10));
    expect(problem("json", '{"a": undefined}')).toEqual(at(1, 7));
    expect(problem("json", "{\"a\": 'x'}")).toEqual(at(1, 7));
    expect(problem("json", '{"a": .5}')).toEqual(at(1, 7));
    expect(problem("json", '{"a": +1}')).toEqual(at(1, 7));
    expect(problem("json", '{"a": 1 nul}')).toEqual(at(1, 9));
    expect(problem("json", '{"a": 1, nul}')).toEqual(at(1, 10));
  });

  it("points at the last character when the text ends early, as the server does", () => {
    expect(problem("json", "{\n")).toEqual(at(1, 2));
    expect(problem("json", "{")).toEqual(at(1, 1));
    expect(problem("json", "[")).toEqual(at(1, 1));
    expect(problem("json", '{"a":1\n\n')).toEqual(at(2, 1));
    expect(problem("json", '{"a":1,')).toEqual(at(1, 7));
    expect(problem("json", '{"a":"b"')).toEqual(at(1, 8));
    expect(problem("json", '{"a": tr')).toEqual(at(1, 8));
    expect(problem("json", "tru")).toEqual(at(1, 3));
    for (const cutShort of ['{"a":"cafe', '{"a":"caf\u00E9', '{"a":"caf\u{1F600}']) {
      expect(problem("json", cutShort), JSON.stringify(cutShort)).toEqual(at(1, 10));
    }
  });

  it("uses the server's words for a trailing comma", () => {
    for (const trailing of ['{"a":1,}', "[1,]", '{"a":[1,\r\n],"b":2}', '{"a":1 , }']) {
      expect(problem("json", trailing)?.error, JSON.stringify(trailing)).toBe("trailing comma");
    }
    expect(problem("json", "[1,]")).toEqual(at(1, 4));
    expect(problem("json", '{"a":1,,"b":2}')?.error).not.toBe("trailing comma");
    expect(problem("json", '{"a":1,')?.error).not.toBe("trailing comma");
  });

  it("drops the position and source from V8's message", () => {
    expect(problem("json", '{"a":NaN}')?.error).toBe("Unexpected token 'N'");
    expect(problem("json", "[")?.error).toBe("Unexpected end of JSON input");
    expect(problem("json", "{}}")?.error).toBe("Unexpected non-whitespace character after JSON");
    expect(problem("json", '{"a":1}\n{"b":2}\n')?.error).toBe(
      "Unexpected non-whitespace character after JSON",
    );
  });

  it("reads Firefox's line and column and falls back to the syntax tree", () => {
    expect(
      jsonErrorIn(
        '{"a":1\r"b":2}',
        "JSON.parse: expected ',' or '}' after property value in object at line 2 column 1 of the JSON data",
      ),
    ).toEqual({ at: 7, error: "expected ',' or '}' after property value in object" });
    expect(jsonErrorIn("{\n  x\n}", 'JSON Parse error: Unexpected identifier "x"')).toEqual({
      at: 4,
      error: 'Unexpected identifier "x"',
    });
    expect(jsonErrorIn("[", "JSON Parse error: Unexpected EOF")).toEqual({
      at: 0,
      error: "Unexpected EOF",
    });
    expect(
      jsonErrorIn(
        '{"a": ture}',
        "JSON.parse: unexpected keyword at line 1 column 7 of the JSON data",
      ),
    ).toEqual({ at: 7, error: "unexpected keyword" });
    expect(
      jsonErrorIn(
        "{\n",
        "JSON.parse: end of data while reading object contents at line 2 column 1 of the JSON data",
      ),
    ).toEqual({ at: 1, error: "end of data while reading object contents" });
  });
});

describe("TOML 1.1, the way Codex reads config.toml", () => {
  it("accepts TOML 1.1, a BOM and CRLF", () => {
    for (const valid of [
      "",
      "  \n\n",
      '\uFEFFmodel = "gpt"\r\n[features]\r\n\tx = true\r\n',
      'a = {\n  b = 1,\n  c = 2,\n}\ns = "\\e\\x41"\nt = 07:32\nd = 1979-05-27T07:32Z\n',
      '[projects."/home/dev/projects/a"]\ntrust_level = "trusted"',
      "a = 9223372036854775807\nb = -9223372036854775808\nc = 9007199254740992\n",
      "a = 0x7FFFFFFFFFFFFFFF\n",
    ]) {
      expect(problem("toml", valid), JSON.stringify(valid)).toBeUndefined();
    }
  });

  it("rejects duplicates at the server's line and character column", () => {
    expect(problem("toml", "a = 1\na = 2\n")).toEqual(at(2, 1));
    expect(problem("toml", "[a]\n[a]\n")).toEqual(at(2, 2));
    expect(problem("toml", "\uFEFFa = 1\r\na = 2\r\n")).toEqual(at(2, 1));
    expect(problem("toml", 'a = "\u00E9\u{1F600}" b = 1\n')).toEqual(at(1, 10));
  });

  it("keeps only the first line of smol-toml's message", () => {
    const message = problem("toml", 'a = "x" b = 1\n')?.error;
    expect(message).toBe("each key-value declaration must be followed by an end-of-line");
  });

  it("uses the server's words for a duplicate key", () => {
    for (const duplicate of ["a = 1\na = 2\n", "[a]\n[a]\n", "a = {b = 1, b = 2}\n"]) {
      expect(problem("toml", duplicate)?.error, JSON.stringify(duplicate)).toBe("duplicate key");
    }
  });

  it("differs from Codex on these, where the server's answer wins", () => {
    for (const leapSecond of [
      "t = 07:32:60\n",
      "d = 1979-05-27T23:59:60Z\n",
      "t = 07:32:60.5\n",
      "d = 1979-12-31T23:59:60+01:00\n",
    ]) {
      expect(problem("toml", leapSecond), JSON.stringify(leapSecond)).toEqual(at(1, 5));
    }
    for (const overflow of ["a = 9223372036854775808\n", "a = -9223372036854775809\n"]) {
      expect(problem("toml", overflow), JSON.stringify(overflow)).toBeUndefined();
    }
    for (const noSuchDay of ["d = 1979-02-30\n", "d = 1900-02-29\n", "d = 1979-04-31\n"]) {
      expect(problem("toml", noSuchDay), JSON.stringify(noSuchDay)).toBeUndefined();
    }
    expect(problem("toml", "a = 1\rb = 2\n")).toEqual(at(1, 6));
    expect(problem("toml", "[a]\nb.c = 1\n[a.b]\nd = 2\n")).toEqual(at(3, 2));
    expect(
      problem("toml", '[projects."/p"]\ntrust_level = "a"\n[projects."/p"]\ntrust_level = "b"\n'),
    ).toEqual(at(3, 2));
  });
});

describe("offsetOf", () => {
  it("turns the server's character column back into an offset", () => {
    const doc = docOf('{\r\n"\u{1F600}\u00E9": x}\r\n');
    const offset = offsetOf(doc, { line: 2, column: 7 });
    expect(doc.sliceString(offset, offset + 1)).toBe("x");
    expect(parseProblemAt(doc, { at: offset, error: "" })).toEqual(at(2, 7));
  });

  it("stays inside the document", () => {
    const doc = docOf("{}\n");
    expect(offsetOf(doc, { line: 9, column: 9 })).toBe(3);
    expect(offsetOf(doc, { line: 1, column: 99 })).toBe(2);
    expect(offsetOf(doc, { line: 0, column: 0 })).toBe(0);
  });
});
