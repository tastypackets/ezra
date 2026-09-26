import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

function card(page: Page, title: string) {
  return page.locator("section", {
    has: page.getByRole("heading", { name: title, exact: true, level: 2 }),
  });
}

test("the Claude release channel is saved", async ({ page }) => {
  await page.goto("./");
  await page.getByRole("link", { name: "Settings" }).click();
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
