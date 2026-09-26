import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

function card(page: Page, title: string) {
  return page.locator("section", {
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
  await expect(claude.getByRole("status")).toHaveText("Saved");

  await page.reload();
  await expect(stable).toBeChecked();
  await latest.click();
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(claude.getByRole("status")).toHaveText("Saved");
});

test("Remote Control settings are saved and shown on the dashboard", async ({ page }) => {
  await page.goto("./settings");
  const claude = card(page, "Claude Code");
  const serve = claude.getByRole("switch", { name: "Serve /projects" });
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
  await expect(claude.getByRole("status")).toHaveText("Saved");

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
  await expect(claude.getByRole("status")).toHaveText("Saved");
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
  await expect(git.getByRole("status")).toHaveText("Saved");

  await page.reload();
  await expect(git.getByLabel("Name")).toHaveValue("Ada Lovelace");
  await expect(git.getByLabel("Email")).toHaveValue("ada@example.com");
});
