import { expect, test } from "@playwright/test";

import { inContainer } from "./manager.ts";

const SIGNED_IN = "/tmp/e2e-codex-signed-in";
const FAKE_VERSION = "/home/dev/.local/share/codex/9.9.9";
const FAKE_CODEX = `${FAKE_VERSION}/bin/codex`;
const CODEX_COMMAND = "/home/dev/.local/bin/codex";

/** A `codex` whose device sign-in finishes once the marker file exists. */
const FAKE_CODEX_SCRIPT = `#!/bin/sh
case "$*" in
  "login --device-auth")
    echo 'Open https://auth.openai.com/codex/device'
    echo 'Enter this one-time code'
    echo 'ABCD-12345'
    while [ ! -f ${SIGNED_IN} ]; do sleep 0.2; done ;;
  "login status")
    if [ -f ${SIGNED_IN} ]; then echo 'Logged in using ChatGPT' >&2; else echo 'Not logged in' >&2; exit 1; fi ;;
  logout) rm -f ${SIGNED_IN} ;;
esac
`;

test("a Codex sign-in and sign-out show up without a refresh", async ({ page }) => {
  inContainer(
    "sh",
    "-c",
    'mkdir -p "$(dirname "$1")" "$(dirname "$2")" && printf "%s" "$3" > "$1" && chmod +x "$1" && ln -sfn "$1" "$2"',
    "sh",
    FAKE_CODEX,
    CODEX_COMMAND,
    FAKE_CODEX_SCRIPT,
  );
  try {
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
  } finally {
    // The brackets keep the pattern from matching this shell's own command line.
    inContainer(
      "sh",
      "-c",
      'pkill -f "[c]odex login --device-auth"; rm -rf "$1" "$2" "$3"',
      "sh",
      FAKE_VERSION,
      CODEX_COMMAND,
      SIGNED_IN,
    );
  }
});
