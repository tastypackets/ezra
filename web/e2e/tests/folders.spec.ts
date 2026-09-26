import { randomUUID } from "node:crypto";

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { inContainer } from "./manager.ts";

const COMMIT = ["-c", "user.name=E2E", "-c", "user.email=e2e@example.com", "commit", "--quiet"];

/** A repository with one commit and nothing pushed. */
function makeRepository(path: string): void {
  inContainer("git", "init", "--quiet", path);
  inContainer("git", "-C", path, ...COMMIT, "--allow-empty", "--message=first");
}

function folderRow(page: Page, folder: string) {
  return page.getByRole("listitem").filter({ hasText: folder });
}

test("a repository is cloned from its URL and deleted after confirming", async ({ page }) => {
  const source = `/tmp/source-${randomUUID().slice(0, 8)}`;
  const folder = `cloned-${randomUUID().slice(0, 8)}`;
  makeRepository(source);
  try {
    await page.goto("./");
    await page.getByRole("button", { name: "Clone repository" }).click();
    const dialog = page.getByRole("dialog", { name: "Clone repository" });
    const serve = dialog.getByRole("switch", { name: "Serve in the Claude app" });
    await expect(serve).toBeChecked();
    await dialog.getByRole("combobox", { name: "Repository" }).fill(`file://${source}`);
    await expect(dialog.getByLabel("Folder")).toHaveValue(source.split("/").at(-1) ?? "");
    await dialog.getByLabel("Folder").fill(folder);
    await serve.click();
    await dialog.getByRole("button", { name: "Clone", exact: true }).click();
    await expect(dialog).toBeHidden();

    const row = folderRow(page, folder);
    await expect(
      row.getByRole("switch", { name: `Serve ${folder} in the Claude app` }),
    ).not.toBeChecked();

    await row.getByRole("button", { name: `More ${folder} actions` }).click();
    await page.getByRole("menuitem", { name: "Delete" }).click();
    const confirm = page.getByRole("alertdialog", { name: `Delete ${folder}?` });
    await expect(confirm).toContainText(`Deletes /projects/${folder} and everything in it.`);
    await expect(confirm.getByText("Checking for unsaved work")).toBeHidden();
    await expect(confirm.getByText("Only in this folder:")).toBeHidden();
    await confirm.getByRole("button", { name: "Delete" }).click();
    await expect(page.getByText(`Deleted ${folder}.`)).toBeVisible();
    await expect(row).toBeHidden();
  } finally {
    inContainer("rm", "-rf", source, `/projects/${folder}`);
  }
});

test("deleting a repository names the work only it has", async ({ page }) => {
  const folder = `unsaved-${randomUUID().slice(0, 8)}`;
  const path = `/projects/${folder}`;
  makeRepository(path);
  inContainer("touch", `${path}/notes.txt`);
  try {
    await page.goto("./");
    const row = folderRow(page, folder);
    await row.getByRole("button", { name: `More ${folder} actions` }).click();
    await page.getByRole("menuitem", { name: "Delete" }).click();
    const confirm = page.getByRole("alertdialog", { name: `Delete ${folder}?` });
    await expect(confirm.getByText("Only in this folder:")).toBeVisible();
    await expect(confirm.getByRole("listitem")).toHaveText([
      "1 uncommitted change",
      "1 commit on no remote",
    ]);

    await confirm.getByRole("button", { name: "Cancel" }).click();
    await expect(confirm).toBeHidden();
    await expect(row).toBeVisible();
  } finally {
    inContainer("rm", "-rf", path);
  }
});

test("a failed clone says why until dismissed", async ({ page }) => {
  const folder = `missing-${randomUUID().slice(0, 8)}`;
  await page.goto("./");
  await page.getByRole("button", { name: "Clone repository" }).click();
  const dialog = page.getByRole("dialog", { name: "Clone repository" });
  await dialog.getByRole("combobox", { name: "Repository" }).fill(`/tmp/${folder}`);
  await dialog.getByRole("button", { name: "Clone", exact: true }).click();

  const row = folderRow(page, folder);
  await expect(row).toContainText("Could not clone");
  await expect(row).toContainText("does not exist");
  await row.getByRole("button", { name: `Dismiss the failed clone of ${folder}` }).click();
  await expect(row).toBeHidden();
});
