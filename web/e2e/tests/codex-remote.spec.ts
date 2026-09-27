import { expect, test } from "@playwright/test";
import type { APIRequestContext, Locator, Page } from "@playwright/test";

import {
  CODEX_SIGNED_OUT,
  installFakeCodex,
  nudgeCodex,
  recordApiCalls,
  removeFakeCodex,
} from "./manager.ts";

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
const NOT_CHATGPT = { state: "waiting", problem: "not_chatgpt", restarts: 0 };
const HELD_FOR_SIGN_IN = { state: "waiting", restarts: 0 };
const CONNECTED = {
  state: "running",
  relay: "connected",
  restarts: 0,
  server_name: "ezra-e2e",
  server_version: "9.9.9-stubbed",
  usage: { chats: 2, running_chats: 1, memory_bytes: 150_000_000 },
};
const PAIRING = "/api/v1/remote-control/codex/pairing";
const PAIRING_QR = "/api/v1/remote-control/codex/pairing/qr.svg";
const PHONES = "/api/v1/remote-control/codex/phones";
const QR_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1" fill="#fff"/></svg>';
const UNTIL_ENROLLED = "remote control pairing is unavailable until enrollment completes";
const DEFERRED = "remote control retry deferred until 2026-09-27T18:00:00Z";
const PICKER_BLOCKED = "The ChatGPT app's folder picker is expected to fail in this container.";
const PHONE_LIST = [
  {
    id: "phone-1",
    name: "Zeke's iPhone",
    model: "iPhone17,1",
    last_seen_at: "2026-09-27T17:00:00Z",
  },
  { id: "phone-2", model: "Pixel 9", platform: "Android" },
  { id: "phone-3" },
];

type Pairing = Record<string, unknown>;

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

/** A pairing code Codex gave, with ten minutes left. */
function pairingCode(code: string): Pairing {
  return {
    manual_code: code,
    link: `https://chatgpt.com/codex/pair?pairing_code=${code}`,
    expires_at: new Date(Date.now() + 10 * 60_000).toISOString(),
    state: "open",
  };
}

/**
 * Answers each pairing POST with `next()` once `held` settles, and each GET with the latest code in
 * whatever state the test gives it, and serves a QR code for it.
 */
async function stubPairing(page: Page, next: () => Pairing, held?: Promise<void>) {
  const stub: { latest: Pairing | null; posts: number } = { latest: null, posts: 0 };
  await page.route(
    (url) => url.pathname === PAIRING,
    async (route) => {
      if (route.request().method() === "POST") {
        stub.posts += 1;
        await held;
        stub.latest = next();
        return route.fulfill({ json: stub.latest });
      }
      return route.fulfill({ json: { pairing: stub.latest } });
    },
  );
  await page.route(
    (url) => url.pathname === PAIRING_QR,
    (route) => route.fulfill({ contentType: "image/svg+xml", body: QR_SVG }),
  );
  return stub;
}

/** Serves `phones` as the paired phones, and drops one on its DELETE. */
async function stubPhones(page: Page, phones: Pairing[]) {
  let listed = phones;
  await page.route(
    (url) => url.pathname.startsWith(PHONES),
    (route) => {
      if (route.request().method() === "DELETE") {
        const id = new URL(route.request().url()).pathname.split("/").at(-1);
        listed = listed.filter((phone) => phone["id"] !== id);
        return route.fulfill({ status: 204 });
      }
      return route.fulfill({ json: listed });
    },
  );
}

async function chooseCodexAction(page: Page, codex: Locator, action: string): Promise<void> {
  await codex.getByRole("button", { name: "More Codex actions" }).click();
  await page.getByRole("menuitem", { name: action }).click();
}

async function pageOverflow(page: Page): Promise<number> {
  return page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
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

  installFakeCodex("9.9.9-signedout", CODEX_SIGNED_OUT);
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

  test("a Codex fix stays busy while its sign-in waits for Codex to stop", async ({
    page,
    request,
  }) => {
    const { promise: held, resolve: release } = Promise.withResolvers<void>();
    let codex: object = NOT_CHATGPT;
    await page.route("**/api/v1/agents/codex/login", async (route) => {
      codex = HELD_FOR_SIGN_IN;
      const on = await request.put("api/v1/agents/codex/settings", { data: CODEX_DEFAULTS });
      expect(on.ok()).toBe(true);
      await held;
      return route.fulfill({ json: { url: "https://example.com/device", code: "ABCD-1234" } });
    });
    await page.goto("./");
    const row = codexRow(page);
    await expect(row).toContainText("Signed in");
    await stubCodexStatus(page, request, () => codex);
    const signIn = row.getByRole("button", { name: "Sign in with ChatGPT" });
    await signIn.click();
    const confirm = page.getByRole("alertdialog", { name: "Sign in to Codex again?" });
    await confirm.getByRole("button", { name: "Sign in again" }).click();

    await expect(remoteCell(row)).toHaveText("Waiting");
    await expect(signIn).toBeVisible();
    await expect(signIn).toHaveAttribute("aria-busy", "true");
    release();
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

  test("Pair a phone says why while Codex is not connected and asks for no code", async ({
    page,
    request,
  }) => {
    const apiCalls = recordApiCalls(page);
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => MFA);
    await expect(remoteCell(row)).toContainText("Needs MFA");

    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog).toContainText("Needs MFA");
    await expect(dialog).toContainText(
      "Turn on multi-factor authentication in ChatGPT, then try again.",
    );
    await expect(dialog).toHaveAccessibleDescription(/Pairing needs Codex connected to ChatGPT\.$/);
    expect(apiCalls.filter((call) => call.includes(PAIRING))).toEqual([]);
  });

  test("Pair a phone follows Codex once it connects and then offers a code", async ({
    page,
    request,
  }) => {
    let codex: object = MFA;
    const pairing = await stubPairing(page, () => pairingCode("E2E-4821"));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => codex);
    await expect(remoteCell(row)).toContainText("Needs MFA");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog).toContainText("Needs MFA");

    codex = CONNECTED;
    const on = await request.put("api/v1/agents/codex/settings", { data: CODEX_DEFAULTS });
    expect(on.ok()).toBe(true);
    const newCode = dialog.getByRole("button", { name: "New code" });
    await expect(newCode).toBeVisible();
    await expect(dialog).not.toContainText("Needs MFA");
    await expect(dialog).not.toContainText("Pairing needs Codex connected to ChatGPT.");
    expect(pairing.posts).toBe(0);

    await newCode.click();
    await expect(dialog.getByText("E2E-4821", { exact: true })).toBeVisible();
    expect(pairing.posts).toBe(1);
  });

  test("Pair a phone shows the wait for a code and takes no second click", async ({
    page,
    request,
  }) => {
    const { promise: held, resolve: release } = Promise.withResolvers<void>();
    const pairing = await stubPairing(page, () => pairingCode("E2E-4821"), held);
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");

    await chooseCodexAction(page, row, "Pair a phone");
    await expect(
      row.getByRole("status").filter({ hasText: "Getting a pairing code" }),
    ).toBeVisible();
    await row.getByRole("button", { name: "More Codex actions" }).click();
    await expect(page.getByRole("menuitem", { name: "Pair a phone" })).toBeDisabled();
    await page.keyboard.press("Escape");

    release();
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("E2E-4821", { exact: true })).toBeVisible();
    await expect(row.getByText("Getting a pairing code")).toBeHidden();
    expect(pairing.posts).toBe(1);
  });

  test("Pair a phone shows a code to enter, its time left, the box's name and a QR code", async ({
    page,
    request,
  }) => {
    const pairing = await stubPairing(page, () => pairingCode("E2E-4821"));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");

    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("Enter this code in the ChatGPT app")).toBeVisible();
    await expect(dialog.getByText("E2E-4821", { exact: true })).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Copy" })).toBeFocused();
    await expect(dialog.getByText(/^Expires in (10:00|9:\d\d)$/)).toBeVisible();
    await expect(dialog.getByText("Waiting for the phone")).toBeVisible();
    await expect(dialog.getByText("Name: ezra-e2e")).toBeVisible();
    await expect(dialog.getByText(PICKER_BLOCKED)).toBeHidden();
    await expect(dialog.getByRole("img", { name: "QR code for pairing" })).toBeHidden();

    await dialog.getByRole("button", { name: "Show QR code" }).click();
    const qr = dialog.getByRole("img", { name: "QR code for pairing" });
    await expect(qr).toBeVisible();
    await expect(qr).toHaveAttribute("src", new RegExp(`^${PAIRING_QR}\\?v=`));
    await expect(dialog.getByRole("button", { name: "Hide QR code" })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    await expect(dialog.getByRole("link", { name: "Open in the ChatGPT app" })).toHaveAttribute(
      "href",
      "https://chatgpt.com/codex/pair?pairing_code=E2E-4821",
    );

    await dialog.getByRole("button", { name: "Close" }).click();
    await expect(dialog).toBeHidden();
    await chooseCodexAction(page, row, "Pair a phone");
    await expect(dialog.getByText("E2E-4821", { exact: true })).toBeVisible();
    expect(pairing.posts).toBe(1);
  });

  test("a phone using the code closes the dialog with one toast", async ({ page, request }) => {
    const pairing = await stubPairing(page, () => pairingCode("E2E-4821"));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("E2E-4821", { exact: true })).toBeVisible();

    pairing.latest = { ...pairing.latest, state: "claimed" };
    await expect(dialog).toBeHidden();
    await expect(page.getByText("Phone paired.")).toHaveCount(1);
  });

  test("a phone using the code after the dialog closed still shows one toast, on any page", async ({
    page,
    request,
  }) => {
    const pairing = await stubPairing(page, () => pairingCode("E2E-4821"));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("E2E-4821", { exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: "Close" }).click();
    await expect(dialog).toBeHidden();
    await page.getByRole("link", { name: "Settings", exact: true }).click();
    await expect(page.getByRole("radiogroup", { name: "Sandbox" })).toBeVisible();

    pairing.latest = { ...pairing.latest, state: "claimed" };
    await expect(page.getByText("Phone paired.")).toHaveCount(1);
    await expect(dialog).toBeHidden();
  });

  test("a code with no manual code shows its QR code at once", async ({ page, request }) => {
    await stubPairing(page, () => ({ ...pairingCode("E2E-QR"), manual_code: null }));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByRole("img", { name: "QR code for pairing" })).toBeVisible();
    await expect(dialog.getByRole("link", { name: "Open in the ChatGPT app" })).toHaveAttribute(
      "href",
      "https://chatgpt.com/codex/pair?pairing_code=E2E-QR",
    );
    await expect(dialog.getByRole("button", { name: "Show QR code" })).toBeHidden();
    await expect(dialog.getByRole("button", { name: "Copy" })).toBeHidden();
    await expect(dialog.getByText(/^Expires in (10:00|9:\d\d)$/)).toBeVisible();
    await expect(dialog.getByText("Waiting for the phone")).toBeVisible();
  });

  test("an expired code offers a new one", async ({ page, request }) => {
    let code = "E2E-0001";
    const pairing = await stubPairing(page, () => pairingCode(code));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("E2E-0001", { exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: "Show QR code" }).click();
    const qr = dialog.getByRole("img", { name: "QR code for pairing" });
    await expect(qr).toBeVisible();
    const firstQr = await qr.getAttribute("src");

    pairing.latest = { ...pairing.latest, state: "expired" };
    await expect(
      dialog.getByRole("status").filter({ hasText: "This code expired." }),
    ).toBeVisible();
    await expect(dialog.getByText("E2E-0001", { exact: true })).toBeHidden();
    code = "E2E-0002";
    await dialog.getByRole("button", { name: "New code" }).click();
    await expect(dialog.getByText("E2E-0002", { exact: true })).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Copy" })).toBeFocused();
    await expect(dialog.getByText("Waiting for the phone")).toBeVisible();
    await expect(
      dialog.getByRole("status").filter({ hasText: "Waiting for the phone" }),
    ).toHaveCount(0);
    expect(pairing.posts).toBe(2);

    await dialog.getByRole("button", { name: "Show QR code" }).click();
    await expect(qr).toBeVisible();
    expect(firstQr).toMatch(new RegExp(`^${PAIRING_QR}\\?v=`));
    expect(await qr.getAttribute("src")).not.toBe(firstQr);
  });

  test("a dialog closed while a new code is on its way stays closed", async ({ page, request }) => {
    const { promise: held, resolve: release } = Promise.withResolvers<void>();
    let latest: Pairing = pairingCode("E2E-0001");
    let posts = 0;
    await page.route(
      (url) => url.pathname === PAIRING,
      async (route) => {
        if (route.request().method() !== "POST") {
          return route.fulfill({ json: { pairing: latest } });
        }
        posts += 1;
        await held;
        latest = pairingCode("E2E-0002");
        return route.fulfill({ json: latest });
      },
    );
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("E2E-0001", { exact: true })).toBeVisible();
    expect(posts).toBe(0);
    latest = { ...latest, state: "expired" };
    await dialog.getByRole("button", { name: "New code" }).click();

    await dialog.getByRole("button", { name: "Close" }).click();
    await expect(dialog).toBeHidden();
    const gettingCode = row.getByRole("status").filter({ hasText: "Getting a pairing code" });
    await expect(gettingCode).toBeVisible();
    release();
    await expect(gettingCode).toBeHidden();
    await expect(dialog).toBeHidden();

    await chooseCodexAction(page, row, "Pair a phone");
    await expect(dialog.getByText("E2E-0002", { exact: true })).toBeVisible();
    expect(posts).toBe(1);
  });

  test("a failed check offers a new code, and reopening after a used-up code asks for one", async ({
    page,
    request,
  }) => {
    let code = "E2E-0001";
    const pairing = await stubPairing(page, () => pairingCode(code));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText("E2E-0001", { exact: true })).toBeVisible();

    pairing.latest = { ...pairing.latest, state: "failed", error: DEFERRED };
    await expect(
      dialog.getByRole("status").filter({ hasText: `Could not check the code: ${DEFERRED}` }),
    ).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Copy" })).toBeHidden();
    await expect(dialog.getByRole("button", { name: "New code" })).toBeVisible();

    code = "E2E-0002";
    await dialog.getByRole("button", { name: "Close" }).click();
    await expect(dialog).toBeHidden();
    await chooseCodexAction(page, row, "Pair a phone");
    await expect(dialog.getByText("E2E-0002", { exact: true })).toBeVisible();
    expect(pairing.posts).toBe(2);

    pairing.latest = { ...pairing.latest, state: "expired" };
    await expect(dialog.getByText("This code expired.")).toBeVisible();
    code = "E2E-0003";
    await dialog.getByRole("button", { name: "Close" }).click();
    await expect(dialog).toBeHidden();
    await chooseCodexAction(page, row, "Pair a phone");
    await expect(dialog.getByText("E2E-0003", { exact: true })).toBeVisible();
    expect(pairing.posts).toBe(3);
  });

  test("a code Codex refuses to give shows its reason until a new one comes", async ({
    page,
    request,
  }) => {
    const { promise: held, resolve: release } = Promise.withResolvers<void>();
    let posts = 0;
    await page.route(
      (url) => url.pathname === PAIRING,
      async (route) => {
        if (route.request().method() !== "POST") {
          return route.fulfill({
            json: { pairing: { ...pairingCode("E2E-USED"), state: "claimed" } },
          });
        }
        posts += 1;
        if (posts > 1) {
          await held;
        }
        return route.fulfill({ status: 502, json: { error: UNTIL_ENROLLED } });
      },
    );
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => CONNECTED);
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    const failed = dialog
      .getByRole("status")
      .filter({ hasText: `Could not get a pairing code: ${UNTIL_ENROLLED}` });
    await expect(dialog).toHaveAccessibleDescription(
      `Could not get a pairing code: ${UNTIL_ENROLLED}`,
    );
    await expect(failed).toBeVisible();
    await expect(dialog.getByRole("button", { name: "Copy" })).toBeHidden();
    await expect(dialog.getByText("E2E-USED", { exact: true })).toBeHidden();

    const newCode = dialog.getByRole("button", { name: "New code" });
    await newCode.click();
    await expect(newCode).toBeFocused();
    await expect(dialog).toBeVisible();
    release();
    await expect(failed).toBeVisible();
    await expect(newCode).toBeFocused();
    expect(posts).toBe(2);
  });

  test("Paired phones lists each phone and removes one", async ({ page }) => {
    await stubPhones(page, PHONE_LIST);
    const removed = page.waitForRequest(
      (sent) => sent.method() === "DELETE" && new URL(sent.url()).pathname === `${PHONES}/phone-1`,
    );
    await page.goto("./");
    await chooseCodexAction(page, codexRow(page), "Paired phones");
    const dialog = page.getByRole("dialog", { name: "Paired phones" });
    const phones = dialog.getByRole("listitem");
    await expect(phones).toHaveCount(3);
    await expect(phones.nth(0)).toContainText("Zeke's iPhone");
    await expect(phones.nth(0)).toContainText(/Last seen \S.*\d/);
    await expect(phones.nth(1)).toContainText("Pixel 9");
    await expect(phones.nth(1)).not.toContainText("Last seen");
    await expect(phones.nth(2).getByText("Phone", { exact: true })).toBeVisible();

    const remove = phones.nth(0).getByRole("button", { name: "Remove" });
    await expect(remove).toHaveAccessibleDescription("Zeke's iPhone");
    await remove.click();
    const confirm = page.getByRole("alertdialog", { name: "Remove Zeke's iPhone?" });
    await expect(confirm).toHaveAccessibleDescription(
      "It can no longer reach this box. Pairing it again can fail.",
    );
    await confirm.getByRole("button", { name: "Remove phone" }).click();
    await removed;
    await expect(confirm).toBeHidden();
    await expect(phones).toHaveCount(2);
    await expect(dialog).not.toContainText("Zeke's iPhone");
    await expect(dialog.getByRole("heading", { name: "Paired phones" })).toBeFocused();
    await expect(page.getByText("Phone removed.")).toBeVisible();
  });

  test("a failed removal keeps the phone and says why", async ({ page }) => {
    await stubPhones(page, PHONE_LIST);
    await page.route(
      (url) => url.pathname === `${PHONES}/phone-1`,
      (route) => route.fulfill({ status: 409, json: { error: "Codex is not running" } }),
    );
    await page.goto("./");
    await chooseCodexAction(page, codexRow(page), "Paired phones");
    const dialog = page.getByRole("dialog", { name: "Paired phones" });
    const phones = dialog.getByRole("listitem");
    await phones.nth(0).getByRole("button", { name: "Remove" }).click();
    const confirm = page.getByRole("alertdialog", { name: "Remove Zeke's iPhone?" });
    await confirm.getByRole("button", { name: "Remove phone" }).click();
    await expect(confirm.getByRole("alert")).toHaveText("Codex is not running");
    await expect(confirm).toBeVisible();
    await confirm.getByRole("button", { name: "Cancel" }).click();
    await expect(confirm).toBeHidden();
    await expect(phones).toHaveCount(3);
    await expect(phones.nth(0)).toContainText("Zeke's iPhone");
  });

  test("Paired phones says when there are none, and why it cannot list them", async ({ page }) => {
    await page.goto("./");
    await chooseCodexAction(page, codexRow(page), "Paired phones");
    const dialog = page.getByRole("dialog", { name: "Paired phones" });
    await expect(dialog.getByRole("alert")).toHaveText("Codex is not running");
    await dialog.getByRole("button", { name: "Close" }).click();
    await expect(dialog).toBeHidden();

    await stubPhones(page, []);
    await chooseCodexAction(page, codexRow(page), "Paired phones");
    await expect(dialog).toContainText("No phones yet.");
  });

  test("a folder picker that fails in this container is noted when pairing and in Settings", async ({
    page,
    request,
  }) => {
    await stubPairing(page, () => pairingCode("E2E-4821"));
    await page.goto("./");
    const row = codexRow(page);
    await stubCodexStatus(page, request, () => ({ ...CONNECTED, folder_picker: "blocked" }));
    await expect(remoteCell(row)).toContainText("Connected");
    await chooseCodexAction(page, row, "Pair a phone");
    const dialog = page.getByRole("dialog", { name: "Pair a phone" });
    await expect(dialog.getByText(PICKER_BLOCKED)).toBeVisible();
    await dialog.getByRole("button", { name: "Close" }).click();

    await page.getByRole("link", { name: "Settings", exact: true }).click();
    await expect(page.getByRole("radiogroup", { name: "Sandbox" })).toHaveAccessibleDescription(
      "Read only and Workspace write fail in this container.",
    );
  });

  test.describe("on a phone", () => {
    test.use({ viewport: { width: 360, height: 780 } });

    test("the pairing dialogs fit the screen", async ({ page, request }) => {
      await stubPairing(page, () => pairingCode("E2E-4821"));
      await stubPhones(page, PHONE_LIST);
      await page.goto("./");
      await stubCodexStatus(page, request, () => ({ ...CONNECTED, folder_picker: "blocked" }));
      const codex = page.getByRole("listitem").filter({ hasText: "Codex" });
      await expect(codex).toContainText("Connected");

      await chooseCodexAction(page, codex, "Pair a phone");
      const pair = page.getByRole("dialog", { name: "Pair a phone" });
      await pair.getByRole("button", { name: "Show QR code" }).click();
      const openInApp = pair.getByRole("link", { name: "Open in the ChatGPT app" });
      await openInApp.scrollIntoViewIfNeeded();
      await expect(openInApp).toBeInViewport({ ratio: 1 });
      await expect(pair.getByRole("img", { name: "QR code for pairing" })).toBeVisible();
      expect(await pair.evaluate((dialog) => dialog.scrollWidth - dialog.clientWidth)).toBe(0);
      expect(await pageOverflow(page)).toBe(0);
      await pair.getByRole("button", { name: "Close" }).click();
      await expect(pair).toBeHidden();

      await chooseCodexAction(page, codex, "Paired phones");
      const phones = page.getByRole("dialog", { name: "Paired phones" });
      const remove = phones.getByRole("button", { name: "Remove" }).first();
      await expect(remove).toBeInViewport({ ratio: 1 });
      expect(await phones.evaluate((dialog) => dialog.scrollWidth - dialog.clientWidth)).toBe(0);
      expect(await pageOverflow(page)).toBe(0);
    });

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
      expect(await pageOverflow(page)).toBe(0);
    });
  });
});
