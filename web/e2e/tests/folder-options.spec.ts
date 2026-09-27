import { expect, test } from "@playwright/test";

import { inContainer, installFakeClaude, removeFakeClaude } from "./manager.ts";

test.describe("with Claude Code installed", () => {
  test.beforeEach(() => installFakeClaude("2.1.0-e2e"));
  test.afterEach(({ request }) => removeFakeClaude(request));

  test("a repository keeps its own Claude Code options", async ({ page }, testInfo) => {
    const folder = `options-${testInfo.retry}`;
    inContainer("git", "init", "--quiet", `/projects/${folder}`);
    try {
      await page.goto("./");
      const row = page.getByRole("listitem").filter({ hasText: folder });
      await row.getByRole("button", { name: `More ${folder} actions` }).click();
      await page.getByRole("menuitem", { name: "Claude Code options" }).click();
      const dialog = page.getByRole("dialog", { name: `Claude Code in ${folder}` });
      await expect(dialog).toContainText("Empty fields follow Settings.");
      const followSettings = dialog.getByRole("radio", { name: "Settings default" });
      await expect(followSettings).toBeChecked();
      await expect(followSettings).toHaveAccessibleDescription("Their own worktree");
      const mode = dialog.getByRole("combobox", { name: "Permission mode" });
      const capacity = dialog.getByRole("spinbutton", { name: "Sessions at once" });
      await expect(mode).toHaveAttribute("placeholder", "Default: auto");
      await expect(capacity).toHaveAttribute("placeholder", "Default: Claude Code's");

      await dialog.getByRole("radio", { name: "The folder" }).click();
      await mode.fill("plan");
      await page.keyboard.press("Escape");
      await capacity.fill("2");
      await dialog.getByRole("button", { name: "Save" }).click();
      await expect(page.getByText(`Saved the Claude Code options for ${folder}.`)).toBeVisible();
      await expect(dialog).toBeHidden();

      const folders = await page.request.get("api/v1/folders");
      expect(await folders.json()).toContainEqual(
        expect.objectContaining({
          name: folder,
          claude: { spawn: "same-dir", permission_mode: "plan", capacity: 2 },
        }),
      );

      await row.getByRole("button", { name: `More ${folder} actions` }).click();
      await page.getByRole("menuitem", { name: "Claude Code options" }).click();
      await expect(dialog.getByRole("radio", { name: "The folder" })).toBeChecked();
      await followSettings.click();
      await dialog.getByRole("button", { name: "Save" }).click();
      await expect(dialog).toBeHidden();
      const followed = await page.request.get("api/v1/folders");
      expect(await followed.json()).toContainEqual(
        expect.objectContaining({
          name: folder,
          claude: { permission_mode: "plan", capacity: 2 },
        }),
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
      await page.getByRole("menuitem", { name: "Claude Code options" }).click();
      const dialog = page.getByRole("dialog", { name: `Claude Code in ${folder}` });
      await expect(dialog).toContainText(
        "Sessions work in the folder, since it is not a git repository.",
      );
      await expect(dialog.getByRole("radio")).toHaveCount(0);

      const refused = await page.request.put(`api/v1/folders/${folder}/claude-options`, {
        data: { spawn: "worktree" },
      });
      expect(refused.status()).toBe(409);
    } finally {
      inContainer("rmdir", `/projects/${folder}`);
    }
  });
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
