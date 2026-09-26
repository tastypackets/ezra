import { expect, test } from "@playwright/test";
import type { Page, PlaywrightTestOptions, PlaywrightWorkerArgs } from "@playwright/test";

import { PASSWORD } from "./manager.ts";

function card(page: Page, title: string) {
  return page.locator("section[data-slot=card]", {
    has: page.getByRole("heading", { name: title, exact: true, level: 2 }),
  });
}

test("the Claude release channel is saved", async ({ page }) => {
  await page.goto("./");
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  const claude = card(page, "Claude Code");
  const latest = claude.getByRole("radio", { name: "Latest" });
  const stable = claude.getByRole("radio", { name: "Stable" });
  await expect(latest).toBeChecked();

  await stable.click();
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();

  await page.reload();
  await expect(stable).toBeChecked();
  await latest.click();
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();
});

test("Remote Control settings are saved and shown on the dashboard", async ({ page }) => {
  await page.goto("./settings");
  const claude = card(page, "Claude Code");
  const serve = claude.getByRole("switch", { name: "Serve to the Claude app" });
  const mode = page.getByRole("combobox", { name: "Permission mode" });
  const capacity = claude.getByLabel("Sessions at once");

  await serve.setChecked(false);
  await mode.fill("plan");
  await mode.press("Enter");
  await expect(mode).toHaveValue("plan");
  await mode.fill("acceptEdits");
  await page.keyboard.press("Escape");
  await capacity.fill("2");
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();

  await page.reload();
  await expect(serve).not.toBeChecked();
  await expect(mode).toHaveValue("acceptEdits");
  await expect(capacity).toHaveValue("2");
  await page.getByRole("link", { name: "Agents", exact: true }).click();
  await expect(page.getByRole("link", { name: "Turned off in Settings." })).toBeVisible();

  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await serve.setChecked(true);
  await mode.fill("auto");
  await page.keyboard.press("Escape");
  await capacity.fill("4");
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();
});

test("a capacity Claude cannot take is refused", async ({ page }) => {
  await page.goto("./settings");
  const claude = card(page, "Claude Code");
  await claude.getByLabel("Sessions at once").fill("40");
  await expect(claude.getByText("Enter a number from 1 to 32.")).toBeVisible();
});

test("git starts signed out of GitHub and keeps the commit identity", async ({ page }) => {
  await page.goto("./settings");
  const git = card(page, "Git");
  await expect(git.getByText("Signed out")).toBeVisible();
  await expect(git.getByRole("button", { name: "Sign in to GitHub" })).toBeVisible();

  await git.getByLabel("Name").fill("Ada Lovelace");
  await git.getByLabel("Email").fill("ada@example.com");
  await git.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Commit identity saved.")).toBeVisible();

  await page.reload();
  await expect(git.getByLabel("Name")).toHaveValue("Ada Lovelace");
  await expect(git.getByLabel("Email")).toHaveValue("ada@example.com");
});

async function signInElsewhere(
  playwright: PlaywrightWorkerArgs["playwright"],
  baseURL: PlaywrightTestOptions["baseURL"],
) {
  const elsewhere = await playwright.request.newContext({ baseURL, ignoreHTTPSErrors: true });
  const signedIn = await elsewhere.post("api/v1/login", { data: { password: PASSWORD } });
  expect(signedIn.status()).toBe(204);
  return elsewhere;
}

test("other sessions can be signed out", async ({ page, playwright, baseURL }) => {
  await page.goto("./settings");
  const manager = card(page, "Manager");
  const elsewhere = await signInElsewhere(playwright, baseURL);
  try {
    const signOutOthers = manager.getByRole("button", { name: "Sign out other sessions" });
    await expect(signOutOthers).toBeEnabled();
    await signOutOthers.click();
    await expect(page.getByText("Other sessions signed out.").last()).toBeVisible();
    await expect(manager.getByText("No other sessions are signed in.")).toBeVisible();
    expect((await elsewhere.get("api/v1/manager")).status()).toBe(401);
  } finally {
    await elsewhere.dispose();
  }
});

test("changing the password needs the current one and signs out other sessions", async ({
  page,
  playwright,
  baseURL,
}) => {
  await page.goto("./settings");
  const manager = card(page, "Manager");
  const current = manager.getByLabel("Current password");
  const next = manager.getByLabel("New password");
  const change = manager.getByRole("button", { name: "Change password" });

  await current.fill("not the password");
  await next.fill("battery staple");
  await change.click();
  await expect(manager.getByText("the current password is wrong")).toBeVisible();
  await expect(current).toHaveAccessibleDescription("the current password is wrong");

  const elsewhere = await signInElsewhere(playwright, baseURL);
  try {
    await current.fill(PASSWORD);
    await next.fill("battery staple");
    await change.click();
    await expect(
      page.getByText("Password changed, other sessions signed out.").last(),
    ).toBeVisible();
    expect((await elsewhere.get("api/v1/manager")).status()).toBe(401);
  } finally {
    await elsewhere.dispose();
  }

  await current.fill("battery staple");
  await next.fill(PASSWORD);
  await change.click();
  await expect(page.getByText("Password changed, other sessions signed out.").last()).toBeVisible();
});

test("the certificate can be regenerated", async ({ page }) => {
  await page.goto("./settings");
  const manager = card(page, "Manager");
  await expect(manager.getByText("localhost")).toBeVisible();
  const fingerprint = manager.getByText(/^([0-9A-F]{2}:){31}[0-9A-F]{2}$/);
  const before = await fingerprint.textContent();

  await manager.getByRole("button", { name: "Regenerate" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Regenerate the certificate?" });
  await expect(dialog).toContainText("Browsers warn again until you accept the new certificate.");
  await dialog.getByRole("button", { name: "Regenerate certificate" }).click();
  await expect(page.getByText("Certificate regenerated.").last()).toBeVisible();
  await expect(fingerprint).not.toHaveText(before ?? "");
});

test("the environment is shown read-only", async ({ page }) => {
  await page.goto("./settings");
  const environment = card(page, "Environment");
  await expect(environment.getByText("EZRA_PORT")).toBeVisible();
  await expect(environment.getByText("8443")).toBeVisible();
  await expect(environment.getByRole("textbox")).toHaveCount(0);
});
