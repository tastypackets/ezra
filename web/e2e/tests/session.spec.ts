import { expect, test } from "@playwright/test";

const PASSWORD = "correct horse";

test("first visitor sets the password, signs out, and signs back in", async ({ page }) => {
  await page.goto("./");
  await expect(page.getByRole("heading", { name: "Set a password" })).toBeVisible();

  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Set password" }).click();
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
  await expect(page.getByRole("row", { name: /Claude Code/ })).toBeVisible();
  await expect(page.getByRole("row", { name: /Codex/ })).toBeVisible();

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();

  await page.getByLabel("Password").fill("wrong");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByText("wrong password")).toBeVisible();

  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
});

test("the first screen arrives with its data", async ({ page }) => {
  const sessionRequests: string[] = [];
  page.on("request", (request) => {
    if (request.url().endsWith("/api/v1/session")) {
      sessionRequests.push(request.url());
    }
  });
  await page.goto("./");
  await expect(page.getByRole("heading", { name: /Set a password|Sign in|Agents/ })).toBeVisible();
  expect(sessionRequests).toEqual([]);
});
