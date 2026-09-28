import { expect, test } from "@playwright/test";

import {
  CODEX_SIGNED_OUT,
  inContainer,
  installFakeCodex,
  removeFakeCodex,
  writeInContainer,
} from "./manager.ts";

const SIGNED_IN = "/tmp/e2e-codex-signed-in";

/** Sign-in branches for a `codex` whose device sign-in finishes once the marker file exists. */
const SIGN_IN = `  "login --device-auth")
    echo 'Open https://auth.openai.com/codex/device'
    echo 'Enter this one-time code'
    echo 'ABCD-12345'
    while [ ! -f ${SIGNED_IN} ]; do sleep 0.2; done ;;
  "login status")
    if [ -f ${SIGNED_IN} ]; then echo 'Logged in using ChatGPT' >&2; else echo 'Not logged in' >&2; exit 1; fi ;;
  logout) rm -f ${SIGNED_IN} ;;`;

test.afterEach(async ({ request }) => {
  // The brackets keep the pattern from matching this shell's own command line.
  inContainer("sh", "-c", 'pkill -f "[c]odex login --device-auth"; rm -f "$1"', "sh", SIGNED_IN);
  await removeFakeCodex(request);
});

test("a Codex sign-in and sign-out show up without a refresh", async ({ page }) => {
  installFakeCodex("9.9.9", SIGN_IN);
  await page.goto("./");
  const row = page.getByRole("row", { name: /Codex/ });
  await expect(row).toContainText("9.9.9");
  await expect(row).toContainText("Signed out");

  await row.getByRole("button", { name: "Sign in" }).click();
  const panel = page.locator("[data-slot=card]", {
    has: page.getByText("Sign in to Codex", { exact: true }),
  });
  await expect(panel.getByText("ABCD-12345")).toBeVisible();

  inContainer("touch", SIGNED_IN);
  await expect(row).toContainText("Signed in");
  await expect(row).toContainText("ChatGPT");
  await expect(panel).toBeHidden();

  await row.getByRole("button", { name: "More Codex actions" }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(row).toContainText("Signed out");
});

test("uninstalling Codex keeps its saved data unless asked to delete it", async ({ page }) => {
  const saved = "/home/dev/.codex/e2e-saved";
  const row = page.getByRole("row", { name: /Codex/ });
  const dialog = page.getByRole("alertdialog", { name: "Uninstall Codex?" });
  const deleteData = dialog.getByRole("switch", {
    name: "Also delete its sign-in, settings and chats",
  });
  try {
    for (const deleting of [true, false]) {
      writeInContainer(saved, "kept");
      installFakeCodex("9.9.9", CODEX_SIGNED_OUT);
      await page.goto("./");
      await expect(row).toContainText("9.9.9");
      await row.getByRole("button", { name: "More Codex actions" }).click();
      await page.getByRole("menuitem", { name: "Uninstall" }).click();
      await expect(dialog).toContainText("Stops its servers and removes the program.");
      await expect(deleteData).not.toBeChecked();
      if (deleting) {
        await deleteData.click();
      }
      await dialog.getByRole("button", { name: "Uninstall" }).click();
      await expect(dialog).toBeHidden();
      await expect(page.getByText("Codex uninstalled.")).toBeVisible();
      await expect(row.getByRole("button", { name: "Install" })).toBeFocused();
      inContainer("test", "!", "-e", "/home/dev/.local/bin/codex");
      expect(inContainer("sh", "-c", 'cat "$1" 2>/dev/null || true', "sh", saved)).toBe(
        deleting ? "" : "kept",
      );
    }
    inContainer("test", "-d", "/home/dev/.codex");
  } finally {
    inContainer("rm", "-f", saved);
  }
});
