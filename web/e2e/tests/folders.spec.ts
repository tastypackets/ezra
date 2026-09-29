import { randomUUID } from "node:crypto";

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import {
  inContainer,
  installFakeClaude,
  installFakeGitHubSignIn,
  nudgeRemoteControl,
  PROJECTS_DIRECTORY,
  removeFakeClaude,
  removeFakeGitHubSignIn,
} from "./manager.ts";

const COMMIT = ["-c", "user.name=E2E", "-c", "user.email=e2e@example.com", "commit", "--quiet"];

/** A repository with one commit and nothing pushed. */
function makeRepository(path: string): void {
  inContainer("git", "init", "--quiet", path);
  inContainer("git", "-C", path, ...COMMIT, "--allow-empty", "--message=first");
}

function folderRow(page: Page, folder: string) {
  return page.getByRole("listitem").filter({ hasText: folder });
}

test("a sibling worktree is counted without becoming a project or Claude server", async ({
  page,
  request,
}) => {
  const id = randomUUID().slice(0, 8);
  const folder = `primary-${id}`;
  const linked = `linked-${id}`;
  const repository = `${PROJECTS_DIRECTORY}/${folder}`;
  const worktree = `${PROJECTS_DIRECTORY}/${linked}`;
  makeRepository(repository);
  installFakeClaude(
    "2.1.0-e2e",
    "echo 'https://claude.ai/code?environment=env_e2e'; exec sleep 600",
  );
  try {
    await nudgeRemoteControl(request);
    await page.goto("./");
    const row = folderRow(page, folder);
    await expect(row.getByText("Running", { exact: true })).toBeVisible();

    inContainer("git", "-C", repository, "worktree", "add", "--quiet", "--detach", worktree);
    await expect(row.getByText("1 worktree", { exact: true })).toBeVisible();
    await expect(folderRow(page, linked)).toBeHidden();

    const folders = await (await request.get("api/v1/folders")).json();
    expect(folders).toContainEqual(
      expect.objectContaining({ name: folder, git: expect.objectContaining({ worktrees: 1 }) }),
    );
    expect(folders).not.toContainEqual(expect.objectContaining({ name: linked }));
    const remote = await (await request.get("api/v1/remote-control")).json();
    expect(remote.folders[folder]).toEqual(expect.objectContaining({ state: "running" }));
    expect(remote.folders).not.toHaveProperty(linked);
    const instructions = inContainer("cat", `${PROJECTS_DIRECTORY}/AGENTS.md`);
    expect(instructions).toContain(`- ${folder}:`);
    expect(instructions).not.toContain(`- ${linked}:`);

    const refused = await request.put(`api/v1/folders/${linked}/remote-control`, {
      data: { serve: true },
    });
    expect(refused.status()).toBe(404);
  } finally {
    await removeFakeClaude(request);
    inContainer("rm", "-rf", repository, worktree);
  }
});

test("a repository is cloned from its URL and deleted after confirming", async ({
  page,
  request,
}) => {
  const source = `/tmp/source-${randomUUID().slice(0, 8)}`;
  const folder = `cloned-${randomUUID().slice(0, 8)}`;
  makeRepository(source);
  installFakeClaude("2.1.0-e2e");
  await installFakeGitHubSignIn(request);
  try {
    await page.goto("./");
    await page.getByRole("button", { name: "Clone repository" }).click();
    const dialog = page.getByRole("dialog", { name: "Clone repository" });
    const serve = dialog.getByRole("switch", { name: "Serve in the Claude app" });
    await expect(serve).toBeChecked();
    await expect(dialog.getByLabel("Folder")).toHaveAccessibleDescription(
      "The new folder in ~/projects.",
    );
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
    await expect(confirm).toContainText(`Deletes ~/projects/${folder} and everything in it.`);
    await expect(confirm.getByText("Checking for unsaved work")).toBeHidden();
    await expect(confirm.getByText("Only in this folder:")).toBeHidden();
    await confirm.getByRole("button", { name: "Delete" }).click();
    await expect(page.getByText(`Deleted ${folder}.`)).toBeVisible();
    await expect(row).toBeHidden();
  } finally {
    inContainer("rm", "-rf", source, `${PROJECTS_DIRECTORY}/${folder}`);
    await removeFakeClaude(request);
    await removeFakeGitHubSignIn(request);
  }
});

test("deleting a repository names the work only it has", async ({ page }) => {
  const folder = `unsaved-${randomUUID().slice(0, 8)}`;
  const path = `${PROJECTS_DIRECTORY}/${folder}`;
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

test("a failed clone says why, can be retried, and is dismissed", async ({ page, request }) => {
  const folder = `missing-${randomUUID().slice(0, 8)}`;
  await installFakeGitHubSignIn(request);
  try {
    await page.goto("./");
    const dialog = page.getByRole("dialog", { name: "Clone repository" });
    const row = folderRow(page, folder);
    for (let attempt = 0; attempt < 2; attempt += 1) {
      await page.getByRole("button", { name: "Clone repository" }).click();
      await dialog.getByRole("combobox", { name: "Repository" }).fill(`/tmp/${folder}`);
      await expect(dialog.getByLabel("Folder")).toHaveValue(folder);
      await dialog.getByRole("button", { name: "Clone", exact: true }).click();
      await expect(dialog).toBeHidden();
      await expect(row).toContainText("Could not clone");
      await expect(row).toContainText("does not exist");
      await expect(row).toHaveCount(1);
    }
    await row.getByRole("button", { name: `Dismiss the failed clone of ${folder}` }).click();
    await expect(row).toBeHidden();
  } finally {
    await removeFakeGitHubSignIn(request);
  }
});

test("without Git set up, the dashboard offers to set it up and scrolls to it", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 600 });
  await page.goto("./");
  await expect(page.getByRole("button", { name: "Clone repository" })).toBeHidden();
  await page.getByRole("link", { name: "Set up Git" }).click();
  await expect(page).toHaveURL(/\/settings#git$/);
  await expect(page.getByRole("heading", { name: "Git", exact: true, level: 2 })).toBeInViewport();
});
