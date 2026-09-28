import { expect, test } from "@playwright/test";
import type { APIResponse, Locator, Page, Request, Route } from "@playwright/test";

import {
  CODEX_SIGNED_OUT,
  inContainer,
  installFakeClaude,
  installFakeCodex,
  nudgeRemoteControl,
  removeFakeClaude,
  removeFakeCodex,
  writeInContainer,
} from "./manager.ts";

const CLAUDE_FILE = "/home/dev/.claude/settings.json";
const CODEX_DIRECTORY = "/home/dev/.codex";
const CODEX_FILE = `${CODEX_DIRECTORY}/config.toml`;
const CLAUDE_API = "**/api/v1/agents/claude/settings-file";
const CLAUDE_TITLE = "Claude Code settings.json";
const CODEX_TITLE = "Codex config.toml";
const CLAUDE_STILL_RUNNING =
  "Remote Control servers keep the old settings. Restarting them ends their running sessions.";
const CHANGED = (path: string) =>
  `${path} changed on disk. Revert loads it, Overwrite replaces it with your text.`;

/** The file's bytes in the container. */
function bytesOf(path: string): Buffer {
  return Buffer.from(inContainer("base64", "-w0", path), "base64");
}

function modeOf(path: string): string {
  return inContainer("stat", "-c", "%a", path).trim();
}

/** Replaces the file the way Claude Code saves it: a temporary file in `.cc-writes/`, renamed into place. */
function writeLikeClaude(path: string, contents: string): void {
  inContainer(
    "sh",
    "-c",
    'mkdir -p "$(dirname "$1")/.cc-writes" && printf "%s" "$2" > "$(dirname "$1")/.cc-writes/.tmp.1.e2e" && mv "$(dirname "$1")/.cc-writes/.tmp.1.e2e" "$1"',
    "sh",
    path,
    contents,
  );
}

/** Replaces the file the way Codex saves it: a `.tmpXXXXXX` file beside it, renamed onto it. */
function writeLikeCodex(path: string, contents: string): void {
  inContainer(
    "sh",
    "-c",
    'mkdir -p "$(dirname "$1")" && printf "%s" "$2" > "$(dirname "$1")/.tmpE2e123" && mv "$(dirname "$1")/.tmpE2e123" "$1"',
    "sh",
    path,
    contents,
  );
}

/** The part of an agent's card that edits the file titled `title`. */
function card(page: Page, title: string) {
  return page.getByRole("region", { name: title, exact: true });
}

const AGENT_OF: Record<string, string> = { [CLAUDE_TITLE]: "Claude Code", [CODEX_TITLE]: "Codex" };

/** Pastes `text` at the cursor, as the clipboard would. */
async function paste(editor: Locator, text: string): Promise<void> {
  await editor.evaluate((element, pasted) => {
    const data = new DataTransfer();
    data.setData("text/plain", pasted);
    element.dispatchEvent(
      new ClipboardEvent("paste", { clipboardData: data, bubbles: true, cancelable: true }),
    );
  }, text);
}

async function openSettings(page: Page, title: string) {
  await page.goto("./settings");
  const box = page.getByRole("region", { name: AGENT_OF[title], exact: true });
  return {
    box,
    editor: box.getByRole("textbox", { name: title }),
    save: box.getByRole("button", { name: "Save", exact: true }),
    overwrite: box.getByRole("button", { name: "Overwrite" }),
    revert: box.getByRole("button", { name: "Revert" }),
  };
}

function savedFile(page: Page) {
  return page.waitForResponse(
    (response) =>
      response.url().endsWith("/settings-file") && response.request().method() === "PUT",
  );
}

/** Holds the page's event stream, so a change reaches the page only when the test sends one. */
async function holdEvents(page: Page): Promise<(topic: string) => Promise<void>> {
  const waiting: Route[] = [];
  await page.route("**/api/v1/events", (route) => {
    waiting.push(route);
  });
  let revision = 0;
  return async (topic) => {
    await expect.poll(() => waiting.length).toBeGreaterThan(0);
    revision += 1;
    await waiting.shift()?.fulfill({
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ event: "changed", topic, revision })}\n\n`,
    });
  };
}

/** Resolves once the page has read Claude's settings file again, or given up on `only`. */
function claudeFileRead(page: Page, only?: Request): Promise<void> {
  return new Promise((resolve) => {
    const settled = (request: Request) => {
      if (
        only
          ? request === only
          : request.method() === "GET" && request.url().endsWith("/claude/settings-file")
      ) {
        page.off("requestfinished", settled);
        page.off("requestfailed", settled);
        resolve();
      }
    };
    page.on("requestfinished", settled);
    page.on("requestfailed", settled);
  });
}

/** Waits for the page to draw twice, so what it last received is on screen. */
async function drawn(page: Page): Promise<void> {
  await page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );
}

test("each agent's settings file shows in its card only once the agent is installed", async ({
  page,
  request,
}) => {
  await page.goto("./");
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  const claude = page.getByRole("region", { name: "Claude Code", exact: true });
  await expect(claude).toBeVisible();
  await expect(card(page, CLAUDE_TITLE)).toHaveCount(0);
  await expect(card(page, CODEX_TITLE)).toHaveCount(0);

  installFakeCodex("9.9.9-e2e", CODEX_SIGNED_OUT);
  try {
    await page.reload();
    const codex = page.getByRole("region", { name: "Codex", exact: true });
    await expect(codex.getByRole("textbox", { name: CODEX_TITLE })).toBeVisible();
    await expect(codex.getByRole("region", { name: CODEX_TITLE })).toBeVisible();
    await expect(card(page, CLAUDE_TITLE)).toHaveCount(0);

    installFakeClaude("2.1.0-e2e");
    await page.reload();
    await expect(claude.getByRole("textbox", { name: CLAUDE_TITLE })).toBeVisible();
    await expect(claude.getByRole("region", { name: CLAUDE_TITLE })).toBeVisible();
  } finally {
    inContainer("rm", "-rf", CODEX_DIRECTORY);
    await removeFakeCodex(request);
    await removeFakeClaude(request);
  }
});

test("a file over 2 MiB is refused", async ({ request }) => {
  const { version } = await (await request.get("api/v1/agents/claude/settings-file")).json();
  const refused = await request.put("api/v1/agents/claude/settings-file", {
    data: { text: `"${"x".repeat(2 * 1024 * 1024)}"`, version },
  });
  expect(refused.status()).toBe(413);
  expect(await refused.json()).toEqual({ error: "the text is larger than 2 MiB" });
});

test("a save while Remote Control runs offers to restart its servers", async ({
  page,
  request,
}) => {
  installFakeClaude(
    "2.1.0-e2e",
    "echo 'https://claude.ai/code?environment=env_e2e'; exec sleep 600",
  );
  await nudgeRemoteControl(request);
  writeInContainer(CLAUDE_FILE, "{}");
  const serverPid = () => inContainer("sh", "-c", "pgrep -xf 'sleep 600' || true").trim();
  try {
    const { box, editor, save } = await openSettings(page, CLAUDE_TITLE);
    const stillRunning = box.getByText(CLAUDE_STILL_RUNNING);
    const restart = box.getByRole("button", { name: "Restart servers" });
    await expect.poll(serverPid).not.toBe("");
    const before = serverPid();
    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"model": "opus"}');
    await expect(stillRunning).toHaveCount(0);
    await save.click();
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();
    await expect(stillRunning).toBeVisible();

    await editor.click();
    await page.keyboard.press("End");
    await page.keyboard.type(" ");
    await expect(stillRunning).toBeHidden();
    await save.click();
    await restart.click();
    await expect(page.getByText("Restarting the Claude Code servers.")).toBeVisible();
    await expect(stillRunning).toBeHidden();
    await expect.poll(serverPid, { timeout: 15_000 }).not.toMatch(new RegExp(`^(${before}|)$`));
  } finally {
    inContainer("rm", "-f", CLAUDE_FILE);
    await removeFakeClaude(request);
  }
});

test.describe("with Claude Code installed", () => {
  test.beforeEach(() => installFakeClaude("2.1.0-e2e"));
  test.afterEach(async ({ request }) => {
    inContainer("rm", "-rf", CLAUDE_FILE, "/home/dev/.claude/.cc-writes", "/config/shared");
    await removeFakeClaude(request);
  });

  test("settings.json is saved byte for byte and keeps its mode", async ({ page }) => {
    const opened =
      '\uFEFF{\r\n\t"model" :  "opus",\r\n\t"unknownKey": {\r\n\t\t"allow": []\r\n\t}\r\n}\r\n\r\n';
    writeInContainer(CLAUDE_FILE, opened, "640");
    const { box, editor, save, revert } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toContainText('"model" :  "opus"');
    await expect(revert).toBeDisabled();
    await expect(save).toBeEnabled();

    await editor.click();
    await page.keyboard.press("Control+Home");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("End");
    await page.keyboard.type(",");
    await page.keyboard.press("Enter");
    await page.keyboard.type('"deny": []');
    await page.keyboard.press("Control+Home");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await page.keyboard.type('"effort": "high",');
    await page.keyboard.press("Control+Home");
    await page.keyboard.press("Enter");
    const saved = savedFile(page);
    await save.click();
    expect((await saved).status()).toBe(200);
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();
    await expect(revert).toBeDisabled();
    await expect(save).toBeFocused();
    await expect(box.getByText(CLAUDE_STILL_RUNNING)).toHaveCount(0);
    expect(bytesOf(CLAUDE_FILE)).toEqual(
      Buffer.from(
        '\uFEFF\r\n{\r\n\t"model" :  "opus",\r\n\t"effort": "high",\r\n\t"unknownKey": {\r\n\t\t"allow": [],\r\n\t\t"deny": []\r\n\t}\r\n}\r\n\r\n',
      ),
    );
    expect(modeOf(CLAUDE_FILE)).toBe("640");

    await page.reload();
    await expect(editor).toContainText('"deny": []');

    writeLikeClaude(CLAUDE_FILE, '{\n\t"model": "opus"\n}\n');
    await expect(editor).not.toContainText('"deny"', { timeout: 10_000 });
    await editor.click();
    await page.keyboard.press("Control+Home");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await page.keyboard.type('"effort": "low",');
    const resaved = savedFile(page);
    await save.click();
    expect((await resaved).status()).toBe(200);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{\n\t"effort": "low",\n\t"model": "opus"\n}\n');
  });

  test("a missing settings.json reads as empty and Save creates it", async ({ page }) => {
    inContainer("rm", "-f", CLAUDE_FILE);
    const { revert, box, editor, save } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toHaveText("");
    await expect(revert).toBeDisabled();

    await editor.click();
    await page.keyboard.type("{,");
    await expect(box.getByText(/^Line 1, column 2: /)).toBeVisible();
    await page.keyboard.press("Control+A");
    await page.keyboard.type("  ");
    await expect(box.getByText(/^Line /)).toBeHidden();
    const blank = savedFile(page);
    await save.click();
    expect((await blank).status()).toBe(200);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe("  ");
    expect(modeOf(CLAUDE_FILE)).toBe("600");

    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"model": "opus"}');
    const saved = savedFile(page);
    await save.click();
    expect((await saved).status()).toBe(200);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{"model": "opus"}');
    expect(modeOf(CLAUDE_FILE)).toBe("600");
  });

  test("invalid JSON blocks Save and shows where it stops parsing", async ({ page }) => {
    writeInContainer(CLAUDE_FILE, "{}\n");
    const { box, editor, save } = await openSettings(page, CLAUDE_TITLE);
    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"a":1,}');
    const problem = box.getByText("Line 1, column 8: Trailing comma");
    await expect(problem).toBeVisible();
    await expect(problem).toHaveAttribute("role", "alert");
    await expect(editor).toHaveAttribute("aria-invalid", "true");
    await expect(editor).toHaveAccessibleDescription("Line 1, column 8: Trailing comma");
    await expect(save).toBeDisabled();

    await page.keyboard.press("Control+Shift+M");
    const errors = box.getByRole("listbox", { name: "Errors" });
    await expect(errors).toBeFocused();
    await expect(errors.getByRole("option")).toHaveText(["Trailing comma"]);
    await page.keyboard.press("Escape");
    await expect(editor).toBeFocused();

    await page.keyboard.press("Control+A");
    await page.keyboard.type("// a comment\n{}");
    await expect(box.getByText(/^Line 1, column 1: /)).toBeVisible();
    await expect(save).toBeDisabled();
    expect(bytesOf(CLAUDE_FILE).toString()).toBe("{}\n");

    await page.keyboard.press("Control+A");
    await page.keyboard.type("[]");
    await expect(problem).toBeHidden();
    await expect(editor).not.toHaveAttribute("aria-invalid");
    await save.click();
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();
    expect(bytesOf(CLAUDE_FILE).toString()).toBe("[]");
  });

  test("a problem's column leaves out the BOM the editor hides", async ({ page }) => {
    writeInContainer(CLAUDE_FILE, '\uFEFF{"a": 1}');
    const { box, editor } = await openSettings(page, CLAUDE_TITLE);
    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.type(",");
    await expect(box.getByText("Line 1, column 9: Trailing comma")).toBeVisible();
  });

  test("a change on disk loads by itself and never replaces unsaved text", async ({ page }) => {
    writeInContainer(CLAUDE_FILE, '{"model": "opus"}\n');
    const { box, editor, save, overwrite, revert } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toContainText('{"model": "opus"}');
    await editor.click();

    writeLikeClaude(CLAUDE_FILE, '{\n  "model": "sonnet"\n}\n');
    await expect(editor).toContainText('"model": "sonnet"', { timeout: 10_000 });
    await expect(editor).toBeFocused();
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);

    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"mine": 1}');
    writeLikeClaude(CLAUDE_FILE, '{\n  "model": "haiku"\n}\n');
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toBeVisible({ timeout: 10_000 });
    await expect(editor).toHaveText('{"mine": 1}');
    await expect(overwrite).toBeEnabled();
    await expect(save).toHaveCount(0);

    await revert.click();
    await expect(editor).toContainText('"model": "haiku"');
    await expect(editor).not.toContainText("mine");
    await expect(editor).toBeFocused();
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);
    await expect(revert).toBeDisabled();

    await page.keyboard.press("Control+Z");
    await expect(editor).toHaveText('{"mine": 1}');
    await expect(revert).toBeEnabled();
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);
    await page.keyboard.press("Control+Y");
    await expect(editor).toContainText('"model": "haiku"');
    await expect(revert).toBeDisabled();

    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"mine": true}');
    writeLikeClaude(CLAUDE_FILE, "{}\n");
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toBeVisible({ timeout: 10_000 });
    await overwrite.click();
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{"mine": true}');
  });

  test("a change on disk keeps the cursor and scroll where they were", async ({ page }) => {
    const keys = Array.from({ length: 60 }, (_, index) => `  "key${index}": ${index},`);
    const opened = `{\n${keys.join("\n")}\n  "last": true\n}\n`;
    writeInContainer(CLAUDE_FILE, opened);
    const { box, editor } = await openSettings(page, CLAUDE_TITLE);
    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("ArrowUp");
    const cursorLine = editor.locator(".cm-activeLine");
    const cursorLineNumber = box.locator(".cm-lineNumbers .cm-activeLineGutter");
    await expect(cursorLine).toHaveText(/"last": true/);
    await expect(cursorLineNumber).toHaveText("62");
    const scrollTop = () => editor.evaluate((content) => content.parentElement?.scrollTop ?? 0);
    const scrolled = await scrollTop();
    expect(scrolled).toBeGreaterThan(0);

    writeLikeClaude(CLAUDE_FILE, opened.replace("{\n", '{\n  "added": 1,\n'));
    await expect(cursorLineNumber).toHaveText("63", { timeout: 10_000 });
    await expect(cursorLine).toHaveText(/"last": true/);
    await expect(editor).toBeFocused();
    expect(await scrollTop()).toBeGreaterThanOrEqual(scrolled);
  });

  test("a save over a file deleted meanwhile is refused, then offered again", async ({ page }) => {
    await page.route("**/api/v1/events", (route) => route.abort());
    writeInContainer(CLAUDE_FILE, '{"model": "opus"}');
    const { box, editor, save, overwrite } = await openSettings(page, CLAUDE_TITLE);
    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"model": "haiku"}');
    inContainer("rm", CLAUDE_FILE);

    const reread = new Promise<Route>((resolve) => {
      void page.route(CLAUDE_API, (route) =>
        route.request().method() === "GET" ? resolve(route) : route.fallback(),
      );
    });
    const refused = savedFile(page);
    await save.click();
    expect((await refused).status()).toBe(409);
    const held = await reread;
    await drawn(page);
    await expect(box.getByText(/changed since it was opened/)).toHaveCount(0);
    await page.unroute(CLAUDE_API);
    await held.continue();
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toBeVisible();
    await expect(box.getByText(/changed since it was opened/)).toHaveCount(0);
    await expect(editor).toContainText('{"model": "haiku"}');
    inContainer("test", "!", "-e", CLAUDE_FILE);

    const saved = savedFile(page);
    await overwrite.click();
    expect((await saved).status()).toBe(200);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{"model": "haiku"}');
  });

  test("a linked settings.json is written through its link", async ({ page }) => {
    writeInContainer("/config/shared/claude.json", '{"model": "opus"}\n', "640");
    inContainer("ln", "-sfn", "/config/shared/claude.json", CLAUDE_FILE);
    const { editor, save } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toContainText('{"model": "opus"}');
    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"model": "sonnet"}');
    await save.click();
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();

    expect(inContainer("readlink", CLAUDE_FILE).trim()).toBe("/config/shared/claude.json");
    expect(bytesOf("/config/shared/claude.json").toString()).toBe('{"model": "sonnet"}');
    expect(modeOf("/config/shared/claude.json")).toBe("640");

    writeLikeClaude("/config/shared/claude.json", '{"model": "haiku"}');
    await expect(editor).toContainText('{"model": "haiku"}', { timeout: 10_000 });
  });

  test("a file that is not UTF-8 is not opened, and never saved over", async ({ page }) => {
    const utf16 = "printf '\\377\\376{\\000}\\000' > \"$1\"";
    inContainer("sh", "-c", utf16, "sh", CLAUDE_FILE);
    const { box, editor } = await openSettings(page, CLAUDE_TITLE);
    await expect(box.getByText(`${CLAUDE_FILE} is not UTF-8 text`)).toBeVisible();
    await expect(editor).toHaveCount(0);

    writeInContainer(CLAUDE_FILE, "{}");
    await box.getByRole("button", { name: "Try again" }).click();
    await expect(editor).toHaveText("{}");
    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"model": "opus"}');
    inContainer("sh", "-c", utf16, "sh", CLAUDE_FILE);
    await expect(box.getByText(`${CLAUDE_FILE} is not UTF-8 text`)).toBeVisible({
      timeout: 10_000,
    });
    await expect(editor).toHaveText('{"model": "opus"}');
    const refused = savedFile(page);
    await box.getByRole("button", { name: "Save", exact: true }).click();
    expect((await refused).status()).toBe(409);
    expect(bytesOf(CLAUDE_FILE)).toEqual(Buffer.from([0xff, 0xfe, 0x7b, 0x00, 0x7d, 0x00]));
  });

  test("a pipe in place of settings.json does not hold up the page", async ({ page }) => {
    inContainer("sh", "-c", 'mkdir -p "$(dirname "$1")" && mkfifo "$1"', "sh", CLAUDE_FILE);
    const { box, editor } = await openSettings(page, CLAUDE_TITLE);
    await expect(box.getByText(`${CLAUDE_FILE} is not a regular file`)).toBeVisible();
    await expect(editor).toHaveCount(0);
    await expect(page.getByRole("heading", { name: "Claude Code", exact: true })).toBeVisible();

    inContainer("rm", CLAUDE_FILE);
    writeInContainer(CLAUDE_FILE, "{}");
    await expect(editor).toHaveText("{}", { timeout: 10_000 });
  });

  test("a save the manager refuses for its size keeps the text and says why", async ({ page }) => {
    writeInContainer(CLAUDE_FILE, "{}");
    const { box, editor, save } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toHaveText("{}");
    await editor.click();
    await page.keyboard.press("Control+A");
    const line = `"${"x".repeat(1_000)}"`;
    await paste(editor, `[\n${Array.from({ length: 2_100 }, () => line).join(",\n")}\n]`);
    const refused = savedFile(page);
    await save.click();
    expect((await refused).status()).toBe(413);
    const notice = box.getByText("the text is larger than 2 MiB");
    await expect(notice).toBeVisible();
    await expect(notice).toHaveAttribute("role", "alert");
    await expect(editor).toContainText("xxxxxxxxxx");
    await expect(save).toBeEnabled();
    expect(bytesOf(CLAUDE_FILE).toString()).toBe("{}");

    await editor.focus();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" ");
    await expect(notice).toBeHidden();
  });

  test("text typed while a save is on its way stays unsaved, with no notice", async ({ page }) => {
    const send = await holdEvents(page);
    writeInContainer(CLAUDE_FILE, "{}");
    const { revert, box, editor, save, overwrite } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toHaveText("{}");
    const put = new Promise<Route>((resolve) => {
      void page.route(CLAUDE_API, (route) =>
        route.request().method() === "PUT" ? resolve(route) : route.fallback(),
      );
    });
    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"a": 1}');
    await save.click();
    const held = await put;
    await page.unroute(CLAUDE_API);
    await editor.focus();
    await page.keyboard.press("End");
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.type("2");
    await expect(editor).toHaveText('{"a": 12}');

    const response = await held.fetch();
    const reread = claudeFileRead(page);
    await send("settings_file");
    await reread;
    await drawn(page);
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);
    await expect(overwrite).toHaveCount(0);
    await held.fulfill({ response });
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{"a": 1}');
    await expect(editor).toHaveText('{"a": 12}');
    await expect(revert).toBeEnabled();

    const echo = claudeFileRead(page);
    await send("settings_file");
    await echo;
    await drawn(page);
    await expect(editor).toHaveText('{"a": 12}');
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);
    const saved = savedFile(page);
    await save.click();
    expect((await saved).status()).toBe(200);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{"a": 12}');
  });

  test("a read that started before a save does not undo it", async ({ page }) => {
    const send = await holdEvents(page);
    writeInContainer(CLAUDE_FILE, '{"model": "opus"}');
    const { revert, box, editor, save } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toHaveText('{"model": "opus"}');
    const stale = new Promise<{ route: Route; response: APIResponse }>((resolve) => {
      void page.route(
        CLAUDE_API,
        async (route) => resolve({ route, response: await route.fetch() }),
        { times: 1 },
      );
    });
    await send("settings_file");
    const { route, response } = await stale;
    const settled = claudeFileRead(page, route.request());

    await editor.click();
    await page.keyboard.press("Control+A");
    await page.keyboard.type('{"model": "haiku"}');
    await save.click();
    await expect(page.getByText(`${CLAUDE_FILE} saved.`).last()).toBeVisible();
    await route.fulfill({ response }).catch(() => undefined);
    await settled;
    await drawn(page);
    await expect(editor).toHaveText('{"model": "haiku"}');
    await expect(revert).toBeDisabled();
    await expect(box.getByText(CHANGED(CLAUDE_FILE))).toHaveCount(0);
    expect(bytesOf(CLAUDE_FILE).toString()).toBe('{"model": "haiku"}');
  });

  test("on a phone the page does not scroll sideways and the text stays literal", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const allowed = Array.from({ length: 40 }, (_, index) => `      "Bash(tool-${index}:*)"`);
    writeInContainer(
      CLAUDE_FILE,
      `{\n  "permissions": {\n    "allow": [\n${allowed.join(",\n")}\n    ]\n  }\n}\n`,
    );
    const { box, editor } = await openSettings(page, CLAUDE_TITLE);
    await editor.scrollIntoViewIfNeeded();
    await expect(editor).toContainText("tool-39");
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      ),
    ).toBe(0);
    expect(
      await editor.evaluate((content) => {
        const scroller = content.parentElement;
        return scroller ? scroller.scrollHeight - scroller.clientHeight : -1;
      }),
    ).toBe(0);
    await editor.click();
    await page.keyboard.press("Control+Home");
    await page.keyboard.type(",");
    await expect(editor).toHaveAttribute("aria-invalid", "true");
    await expect(box.locator(".cm-gutter-lint")).toBeHidden();
    await expect(editor).toHaveCSS("font-size", "16px");
    await expect(editor).toHaveAttribute("spellcheck", "false");
    await expect(editor).toHaveAttribute("autocorrect", "off");
    await expect(editor).toHaveAttribute("autocapitalize", "off");
  });

  test("leaving with unsaved text asks first", async ({ page }) => {
    writeInContainer(CLAUDE_FILE, "{}");
    const { editor } = await openSettings(page, CLAUDE_TITLE);
    await expect(editor).toHaveText("{}");
    const agents = page.getByRole("link", { name: "Agents", exact: true });
    const leave = page.getByRole("alertdialog", { name: "Leave without saving?" });
    await editor.click();
    await page.keyboard.press("End");
    await page.keyboard.type(" ");
    await agents.click();
    await expect(leave).toContainText(`Leaving discards your changes to ${CLAUDE_FILE}.`);
    await leave.getByRole("button", { name: "Stay" }).click();
    await expect(leave).toBeHidden();
    await expect(page).toHaveURL(/\/settings$/);
    await expect(editor).toHaveText("{} ");

    page.once("dialog", (dialog) => void dialog.accept());
    const prompted = page.waitForEvent("dialog");
    await page.reload();
    expect((await prompted).type()).toBe("beforeunload");
    await expect(editor).toHaveText("{}");

    await editor.click();
    await page.keyboard.press("End");
    await page.keyboard.type(" ");
    await agents.click();
    await leave.getByRole("button", { name: "Discard changes" }).click();
    await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
    expect(bytesOf(CLAUDE_FILE).toString()).toBe("{}");
  });

  test("Tab moves focus out of the editor instead of typing", async ({ page }) => {
    writeInContainer(CLAUDE_FILE, "{}");
    const { revert, editor } = await openSettings(page, CLAUDE_TITLE);
    await editor.click();
    await expect(editor).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(editor).not.toBeFocused();
    await page.keyboard.press("Shift+Tab");
    await expect(editor).toBeFocused();
    await expect(revert).toBeDisabled();
    expect(bytesOf(CLAUDE_FILE).toString()).toBe("{}");
  });
});

test.describe("with Codex installed", () => {
  test.beforeEach(() => {
    inContainer("rm", "-rf", CODEX_DIRECTORY);
    installFakeCodex("9.9.9-e2e", CODEX_SIGNED_OUT);
  });
  test.afterEach(async ({ request }) => {
    inContainer("rm", "-rf", CODEX_DIRECTORY);
    await removeFakeCodex(request);
  });

  test("a missing config.toml and its folder are created on save", async ({ page }) => {
    const { box, editor, save } = await openSettings(page, CODEX_TITLE);
    await expect(editor).toHaveText("");
    await editor.click();
    await page.keyboard.type("a =");
    await expect(box.getByText(/^Line 1, /)).toBeVisible();
    await page.keyboard.press("Control+A");
    const toml =
      'model = "o3"\ntui = {\n  notifications = true,\n}\nescape = "\\e\\x41"\nat = 07:32\n';
    await paste(editor, toml);
    await expect(box.getByText(/^Line /)).toBeHidden();
    await save.click();
    await expect(page.getByText(`${CODEX_FILE} saved.`).last()).toBeVisible();
    expect(bytesOf(CODEX_FILE).toString()).toBe(toml);
    expect(modeOf(CODEX_FILE)).toBe("600");
  });

  test("config.toml keeps its BOM, CRLF, tabs and spacing", async ({ page }) => {
    const opened =
      '\uFEFF# mine\r\nmodel = "o3"\t# odd  spacing\r\n[tui]\r\nnotifications   =  true';
    writeInContainer(CODEX_FILE, opened, "644");
    const { editor, save } = await openSettings(page, CODEX_TITLE);
    await expect(editor).toContainText("notifications   =  true");
    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" # yes");
    await page.keyboard.press("Enter");
    await page.keyboard.type('[projects."/home/dev/projects/a"]');
    await page.keyboard.press("Control+Home");
    await page.keyboard.press("End");
    await page.keyboard.press("Enter");
    await page.keyboard.type("# top");
    await save.click();
    await expect(page.getByText(`${CODEX_FILE} saved.`).last()).toBeVisible();
    expect(bytesOf(CODEX_FILE)).toEqual(
      Buffer.from(
        `${opened.replace("# mine\r\n", "# mine\r\n# top\r\n")} # yes\r\n[projects."/home/dev/projects/a"]`,
      ),
    );
    expect(modeOf(CODEX_FILE)).toBe("644");

    writeLikeCodex(CODEX_FILE, '[projects."/home/dev/projects/a"]\ntrust_level = "trusted"\n');
    await expect(editor).toContainText('trust_level = "trusted"', { timeout: 10_000 });
  });

  test("invalid TOML is marked, and the server decides whether it saves", async ({ page }) => {
    const { box, editor, save } = await openSettings(page, CODEX_TITLE);
    await editor.click();
    await paste(editor, "a = 1\na = 2\n");
    const duplicate = box.getByText("Line 2, column 1: Duplicate key");
    await expect(duplicate).toBeVisible();
    await expect(editor).toHaveAttribute("aria-invalid", "true");
    await expect(save).toBeEnabled();
    const duplicateRefused = savedFile(page);
    await save.click();
    expect((await duplicateRefused).status()).toBe(400);
    await expect(duplicate).toBeVisible();
    await expect(save).toBeDisabled();
    inContainer("test", "!", "-e", CODEX_FILE);

    await page.keyboard.press("Control+A");
    await page.keyboard.type("d = 1979-02-28");
    await expect(duplicate).toBeHidden();
    await page.keyboard.press("Backspace");
    await page.keyboard.press("Backspace");
    const malformed = box.getByText(/^Line 1, column 5: Invalid date-time/);
    await expect(malformed).toBeVisible();
    await page.keyboard.type("30");
    await expect(malformed).toBeHidden();
    await expect(editor).not.toHaveAttribute("aria-invalid");
    const refused = savedFile(page);
    await save.click();
    expect((await refused).status()).toBe(400);
    const problem = box.getByText("Line 1, column 5: Invalid date, expected day between 01 and 28");
    await expect(problem).toBeVisible();
    await expect(editor).toBeFocused();
    await expect(editor).toHaveAttribute("aria-invalid", "true");
    await expect(save).toBeDisabled();
    inContainer("test", "!", "-e", CODEX_FILE);

    await page.keyboard.type("X");
    await expect(editor).toHaveText("d = X1979-02-30");
    await expect(problem).toBeHidden();
    await page.keyboard.press("Backspace");
    await expect(editor).toHaveText("d = 1979-02-30");
    await expect(problem).toBeHidden();
    await page.keyboard.press("End");
    await page.keyboard.type("1");
    await page.keyboard.press("Backspace");
    await page.keyboard.type("Z");
    await expect(editor).toHaveText("d = 1979-02-30Z");

    await page.keyboard.press("Control+A");
    await page.keyboard.type("d = 1979-02-28");
    await expect(save).toBeEnabled();
  });

  test("config.toml values only the browser refuses are saved and lose their mark", async ({
    page,
  }) => {
    const opened = "max = 9223372036854775807\nat = 07:32:60\n";
    writeInContainer(CODEX_FILE, opened);
    const { revert, box, editor, save } = await openSettings(page, CODEX_TITLE);
    const hint = box.getByText("Line 2, column 6: Invalid date");
    await expect(hint).toBeVisible();
    await expect(editor).toHaveAttribute("aria-invalid", "true");
    await expect(revert).toBeDisabled();

    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("# mine");
    await expect(revert).toBeEnabled();
    const saved = savedFile(page);
    await save.click();
    expect((await saved).status()).toBe(200);
    expect(bytesOf(CODEX_FILE).toString()).toBe(`${opened}# mine`);
    await expect(hint).toBeHidden();
    await expect(editor).not.toHaveAttribute("aria-invalid");
    await expect(revert).toBeDisabled();
  });

  test("the server's column leaves out the BOM the editor hides", async ({ page }) => {
    writeInContainer(CODEX_FILE, "\uFEFFd = 1979-02-30\n");
    const { box, editor, save } = await openSettings(page, CODEX_TITLE);
    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("# x");
    const refused = savedFile(page);
    await save.click();
    expect((await refused).status()).toBe(400);
    await expect(
      box.getByText("Line 1, column 5: Invalid date, expected day between 01 and 28"),
    ).toBeVisible();
  });

  test("a config.toml created while editing is not overwritten unasked", async ({ page }) => {
    await page.route("**/api/v1/events", (route) => route.abort());
    const { box, editor, save, revert } = await openSettings(page, CODEX_TITLE);
    await editor.click();
    await page.keyboard.type('model = "mine"');
    writeLikeCodex(CODEX_FILE, '[projects."/home/dev/projects/a"]\ntrust_level = "trusted"\n');

    const refused = savedFile(page);
    await save.click();
    expect((await refused).status()).toBe(409);
    await expect(box.getByText(CHANGED(CODEX_FILE))).toBeVisible();
    await expect(editor).toContainText('model = "mine"');
    await revert.click();
    await expect(editor).toContainText('trust_level = "trusted"');
    await expect(editor).not.toContainText("mine");
    await expect(box.getByText(CHANGED(CODEX_FILE))).toHaveCount(0);
    await expect(box.getByText(/changed since it was opened/)).toHaveCount(0);
  });
});
