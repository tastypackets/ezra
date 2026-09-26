import { expect, test as setup } from "@playwright/test";

import { SESSION_FILE } from "../playwright.config.ts";
import { PASSWORD, recordApiCalls } from "./manager.ts";

setup("the first visitor sets the password", async ({ page }) => {
  const apiCalls = recordApiCalls(page);
  await page.goto("./");
  await expect(page).toHaveURL(/\/setup$/);
  await expect(page.getByRole("heading", { name: "Set a password" })).toBeVisible();
  expect(apiCalls).toEqual([]);

  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Set password" }).click();
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
  await page.context().storageState({ path: SESSION_FILE });
});
