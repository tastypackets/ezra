import { expect, test } from "@playwright/test";

test("the Claude release channel is saved", async ({ page }) => {
  await page.goto("./");
  await page.getByRole("link", { name: "Settings" }).click();
  await expect(page.getByRole("heading", { name: "Claude Code" })).toBeVisible();
  const latest = page.getByRole("radio", { name: /^Latest/ });
  const stable = page.getByRole("radio", { name: /^Stable/ });
  await expect(latest).toBeChecked();

  await stable.click();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status")).toHaveText("Saved");

  await page.reload();
  await expect(stable).toBeChecked();
  await latest.click();
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status")).toHaveText("Saved");
});
