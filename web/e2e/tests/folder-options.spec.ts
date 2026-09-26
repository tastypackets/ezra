import { expect, test } from "@playwright/test";

import { inContainer } from "./manager.ts";

test("a repository's new sessions can work in their own worktrees", async ({ page }, testInfo) => {
  const folder = `spawn-${testInfo.retry}`;
  inContainer("git", "init", "--quiet", `/projects/${folder}`);
  try {
    await page.goto("./");
    const row = page.getByRole("listitem").filter({ hasText: folder });
    await row.getByRole("button", { name: `More ${folder} actions` }).click();
    const inFolder = page.getByRole("menuitemradio", { name: /The folder/ });
    const inWorktree = page.getByRole("menuitemradio", { name: /Their own worktree/ });
    await expect(inFolder).toBeChecked();
    await inWorktree.click();
    await expect(page.getByText(`New ${folder} sessions get their own worktree.`)).toBeVisible();
    await expect(inWorktree).toBeChecked();

    const folders = await page.request.get("api/v1/folders");
    expect(await folders.json()).toContainEqual(
      expect.objectContaining({ name: folder, spawn: "worktree" }),
    );
  } finally {
    inContainer("rm", "-rf", `/projects/${folder}`);
  }
});

test("a plain folder's sessions work in the folder", async ({ page }, testInfo) => {
  const folder = `plain-${testInfo.retry}`;
  inContainer("mkdir", `/projects/${folder}`);
  try {
    await page.goto("./");
    const row = page.getByRole("listitem").filter({ hasText: folder });
    await row.getByRole("button", { name: `More ${folder} actions` }).click();
    await expect(page.getByRole("menuitemradio", { name: /The folder/ })).toBeChecked();
    const inWorktree = page.getByRole("menuitemradio", { name: /Their own worktree/ });
    await expect(inWorktree).toBeDisabled();
    await expect(inWorktree).toContainText("Needs a git repository.");

    const refused = await page.request.put(`api/v1/folders/${folder}/spawn-mode`, {
      data: { spawn: "worktree" },
    });
    expect(refused.status()).toBe(409);
  } finally {
    inContainer("rmdir", `/projects/${folder}`);
  }
});

test("a repository's worktrees are counted and git ignores them", async ({ page }, testInfo) => {
  const folder = `worktrees-${testInfo.retry}`;
  const repository = `/projects/${folder}`;
  inContainer("git", "init", "--quiet", repository);
  inContainer(
    "git",
    "-C",
    repository,
    "-c",
    "user.name=e2e",
    "-c",
    "user.email=e2e@example.com",
    "commit",
    "--quiet",
    "--allow-empty",
    "-m",
    "start",
  );
  try {
    await page.goto("./");
    const row = page.getByRole("listitem").filter({ hasText: folder });
    await expect(row).toBeVisible();
    inContainer("git", "-C", repository, "worktree", "add", "--quiet", ".claude/worktrees/feature");
    await expect(row.getByText("1 worktree")).toBeVisible();
    expect(inContainer("git", "-C", repository, "status", "--porcelain")).toBe("");

    inContainer("git", "-C", repository, "worktree", "remove", ".claude/worktrees/feature");
    await expect(row.getByText("1 worktree")).toBeHidden();
  } finally {
    inContainer("rm", "-rf", repository);
  }
});
