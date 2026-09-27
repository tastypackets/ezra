import { expect, test } from "@playwright/test";
import type { APIRequestContext, Locator, Page } from "@playwright/test";

import { installFakeCodex, nudgeCodex, recordApiCalls, removeFakeCodex } from "./manager.ts";

const SIGNED_OUT = `  "login status") echo 'Not logged in' >&2; exit 1 ;;`;
const API_KEY = `  "login status") echo 'Logged in using an API key - sk-proj-***ABCDE' >&2 ;;`;
const CHATGPT = `  "login status") echo 'Logged in using ChatGPT' >&2 ;;`;
const SOCKET_IN_USE = `${CHATGPT}
  app-server*) echo "Error: app-server control socket is already in use at $CODEX_HOME/app-server-control/app-server-control.sock" >&2; exit 1 ;;`;
const CODEX_DEFAULTS = {
  remote_control: { enabled: true, sandbox: "danger-full-access", approvals: "on-request" },
};
const MFA = {
  state: "running",
  relay: "disabled",
  problem: "mfa_required",
  restarts: 0,
  server_name: "ezra-e2e",
  server_version: "9.9.9-stubbed",
};
const SIGN_IN_LOST = {
  state: "running",
  relay: "disabled",
  problem: "signed_out",
  restarts: 0,
  server_name: "ezra-e2e",
  server_version: "9.9.9-stubbed",
  usage: { chats: 1, running_chats: 1, memory_bytes: 150_000_000 },
};
const CONNECTED = {
  state: "running",
  relay: "connected",
  restarts: 0,
  server_name: "ezra-e2e",
  server_version: "9.9.9-stubbed",
  usage: { chats: 2, running_chats: 1, memory_bytes: 150_000_000 },
};

function codexRow(page: Page): Locator {
  return page.getByRole("row", { name: /Codex/ });
}

/** The Remote cell, after the Status, Version and Account cells. */
function remoteCell(row: Locator): Locator {
  return row.getByRole("cell").nth(3);
}

/**
 * Serves `codex()` as Codex's remote control status from now on, and turns Codex's switch off
 * through the API so the page fetches it.
 */
async function stubCodexStatus(
  page: Page,
  request: APIRequestContext,
  codex: () => object,
): Promise<void> {
  await page.route("**/api/v1/remote-control", async (route) => {
    const overview: object = await (await route.fetch()).json();
    await route.fulfill({ json: { ...overview, codex: codex() } });
  });
  const off = await request.put("api/v1/agents/codex/settings", {
    data: { remote_control: { ...CODEX_DEFAULTS.remote_control, enabled: false } },
  });
  expect(off.ok()).toBe(true);
}

test.afterEach(async ({ request }) => {
  try {
    const reset = await request.put("api/v1/agents/codex/settings", { data: CODEX_DEFAULTS });
    expect(reset.ok()).toBe(true);
  } finally {
    await removeFakeCodex(request);
  }
});

test("Codex's Remote cell shows a dash until Codex is installed, then waits for its sign-in", async ({
  page,
  request,
}) => {
  await page.goto("./");
  const row = codexRow(page);
  await expect(row).toContainText("Not installed");
  await expect(remoteCell(row)).toHaveText("—");

  installFakeCodex("9.9.9-signedout", SIGNED_OUT);
  await nudgeCodex(request);
  await page.reload();
  await expect(row).toContainText("Signed out");
  await expect(remoteCell(row)).toHaveText("Waiting");
});

test("a Codex signed in with an API key asks for a ChatGPT sign-in first", async ({
  page,
  request,
}) => {
  installFakeCodex("9.9.9-apikey", API_KEY);
  await nudgeCodex(request);
  const apiCalls = recordApiCalls(page);
  await page.goto("./");
  const row = codexRow(page);
  await expect(remoteCell(row)).toHaveText("Needs a ChatGPT sign-in");

  const signIn = row.getByRole("button", { name: "Sign in with ChatGPT" });
  await expect(signIn).toHaveAccessibleDescription("Codex");
  await signIn.click();
  const confirm = page.getByRole("alertdialog", { name: "Sign in to Codex again?" });
  await expect(confirm).toHaveAccessibleDescription(
    "The current sign-in ends at once, even if the new one does not finish.",
  );
  await expect(confirm.getByRole("button", { name: "Sign in again" })).toBeVisible();
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(confirm).toBeHidden();
  await expect(signIn).toBeFocused();
  expect(apiCalls).not.toContain("POST /api/v1/agents/codex/login");
});

test("another Codex server on the socket is named and logged", async ({ page, request }) => {
  installFakeCodex("9.9.9-socket", SOCKET_IN_USE);
  await nudgeCodex(request);
  await page.goto("./");
  const row = codexRow(page);
  const remote = remoteCell(row);
  await expect(remote).toContainText("Blocked");
  await expect(remote.getByText("Another Codex server was running in this box.")).toBeVisible();

  await row.getByRole("button", { name: "More Codex actions" }).click();
  await page.getByRole("menuitem", { name: "Show log" }).click();
  const log = page.getByRole("dialog", { name: "Codex log" });
  await expect(log).toContainText("/config/ezra/remote-control/codex/server.log");
  await expect(log.getByLabel("Log lines")).toContainText(
    "[OUTPUT] Error: app-server control socket is already in use at",
  );
  await log.getByRole("button", { name: "Close" }).click();
  await expect(log).toBeHidden();
});

test.describe("with Codex signed in with ChatGPT", () => {
  test.beforeEach(async ({ request }) => {
    installFakeCodex("9.9.9-stubbed", CHATGPT);
    await nudgeCodex(request);
  });

  test("Try again asks Codex to connect once ChatGPT wants MFA", async ({ page, request }) => {
    let codex: object = MFA;
    const retried = page.waitForRequest(
      (sent) =>
        sent.method() === "POST" &&
        new URL(sent.url()).pathname === "/api/v1/remote-control/codex/retry",
    );
    await page.route("**/api/v1/remote-control/codex/retry", (route) => {
      codex = CONNECTED;
      return route.fulfill({ status: 204 });
    });
    await page.goto("./");
    const row = codexRow(page);
    await expect(row).toContainText("Signed in");
    await stubCodexStatus(page, request, () => codex);
    const remote = remoteCell(row);
    await expect(remote).toContainText("Needs MFA");
    await expect(
      remote.getByText("Turn on multi-factor authentication in ChatGPT, then try again."),
    ).toBeVisible();

    await row.getByRole("button", { name: "Try again" }).click();
    await retried;
    await expect(remote).toContainText("Connected, 2 chats");
    await expect(row.getByRole("button", { name: "Try again" })).toBeHidden();
  });

  test("a failed Try again shows why until Codex connects", async ({ page, request }) => {
    let codex: object = MFA;
    await page.route("**/api/v1/remote-control/codex/retry", (route) =>
      route.fulfill({ status: 409, json: { error: "Codex is not running" } }),
    );
    await page.goto("./");
    const row = codexRow(page);
    await expect(row).toContainText("Signed in");
    await stubCodexStatus(page, request, () => codex);
    const remote = remoteCell(row);
    const tryAgain = row.getByRole("button", { name: "Try again" });
    await tryAgain.click();
    await expect(row.getByRole("alert")).toHaveText("Codex is not running");
    await expect(remote).toContainText("Needs MFA");
    await expect(tryAgain).toBeVisible();

    codex = CONNECTED;
    const on = await request.put("api/v1/agents/codex/settings", { data: CODEX_DEFAULTS });
    expect(on.ok()).toBe(true);
    await expect(remote).toContainText("Connected, 2 chats");
    await expect(row.getByRole("alert")).toBeHidden();
  });

  test("a Codex that lost its sign-in asks before a new one stops running chats", async ({
    page,
    request,
  }) => {
    await page.route("**/api/v1/agents/codex/login", (route) =>
      route.fulfill({ json: { url: "https://example.com/device", code: "ABCD-1234" } }),
    );
    await page.goto("./");
    const row = codexRow(page);
    await expect(row).toContainText("Signed in");
    await stubCodexStatus(page, request, () => SIGN_IN_LOST);
    await expect(remoteCell(row)).toHaveText("Needs a new sign-in");

    await row.getByRole("button", { name: "Sign in again" }).click();
    const confirm = page.getByRole("alertdialog", { name: "Sign in to Codex again?" });
    await expect(confirm).toHaveAccessibleDescription(
      "The current sign-in ends at once, even if the new one does not finish. Running chats stop too.",
    );
    const signedIn = page.waitForRequest(
      (sent) =>
        sent.method() === "POST" && new URL(sent.url()).pathname === "/api/v1/agents/codex/login",
    );
    await confirm.getByRole("button", { name: "Sign in again" }).click();
    await signedIn;
    await expect(confirm).toBeHidden();
  });

  test("an update waiting for Codex's chats shows under the chat count", async ({
    page,
    request,
  }) => {
    const update = { version: "9.9.10-stubbed", restart_by: "2026-09-27T18:00:00Z" };
    await page.goto("./");
    await stubCodexStatus(page, request, () => ({ ...CONNECTED, update }));
    const remote = remoteCell(codexRow(page));
    await expect(remote).toContainText("Connected, 2 chats");
    await expect(remote.getByText(/^Restarts on Codex 9\.9\.10-stubbed by .+\.$/)).toBeVisible();
  });

  test.describe("on a phone", () => {
    test.use({ viewport: { width: 360, height: 780 } });

    test("Codex's problem and its fix fit the screen", async ({ page, request }) => {
      await page.goto("./");
      await stubCodexStatus(page, request, () => MFA);
      const codex = page.getByRole("listitem").filter({ hasText: "Codex" });
      await expect(
        codex.getByText("Turn on multi-factor authentication in ChatGPT, then try again."),
      ).toBeVisible();
      const tryAgain = codex.getByRole("button", { name: "Try again" });
      await tryAgain.scrollIntoViewIfNeeded();
      await expect(tryAgain).toBeInViewport({ ratio: 1 });
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - window.innerWidth,
      );
      expect(overflow).toBe(0);
    });
  });
});
