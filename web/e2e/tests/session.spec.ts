import { expect, test } from "@playwright/test";

import { PASSWORD, recordApiCalls } from "./manager.ts";

test.use({ storageState: { cookies: [], origins: [] } });

test("a signed-out visitor is sent to sign in", async ({ page }) => {
  const apiCalls = recordApiCalls(page);
  await page.goto("./");
  await expect(page).toHaveURL(/\/login$/);
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  expect(apiCalls).toEqual([]);
});

test("the setup page closes once a password is set", async ({ page }) => {
  await page.goto("./setup");
  await expect(page).toHaveURL(/\/login$/);
});

test("signing in and out", async ({ page }) => {
  await page.goto("./login");
  await page.getByLabel("Password").fill("wrong");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByText("wrong password")).toBeVisible();

  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login$/);
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
});
