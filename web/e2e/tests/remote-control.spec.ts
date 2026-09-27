import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import {
  CLAUDE_CREDENTIALS,
  inContainer,
  installFakeClaude,
  nudgeRemoteControl,
  PROJECTS_DIRECTORY,
  removeFakeClaude,
  writeInContainer,
} from "./manager.ts";

const CONNECTED = "echo 'https://claude.ai/code?environment=env_e2e'; exec sleep 600";
const REJECTED = "echo 'Error: You must be logged in to use Remote Control.' >&2; exit 1";
const BLOCKED =
  "echo 'Error: Remote Control requires claude.ai subscription auth. apiKeyHelper is configured, so this session is using API-key auth — unset it to use Remote Control.' >&2; exit 1";
const DAY_MS = 86_400_000;

function projectsRow(page: Page) {
  return page.getByRole("listitem").filter({ hasText: "All projects" });
}

test.afterEach(async ({ request }) => {
  await removeFakeClaude(request);
});

test("a rejected sign-in is explained, logged and fixed by signing in again", async ({
  page,
  request,
}, testInfo) => {
  const folder = `rejected-${testInfo.retry}`;
  installFakeClaude("2.1.0-e2e", REJECTED);
  await nudgeRemoteControl(request);
  inContainer("git", "init", "--quiet", `${PROJECTS_DIRECTORY}/${folder}`);
  try {
    await page.goto("./");
    const projects = projectsRow(page);
    await expect(
      projects.getByText("Claude Code's sign-in does not work for Remote Control."),
    ).toBeVisible();

    await projects.getByRole("button", { name: "More ~/projects actions" }).click();
    await page.getByRole("menuitem", { name: "Show log" }).click();
    const log = page.getByRole("dialog", { name: "~/projects log" });
    await expect(log).toContainText("/config/ezra/remote-control/projects/server.log");
    await expect(log.getByLabel("Log lines")).toContainText(
      "[OUTPUT] Error: You must be logged in to use Remote Control.",
    );
    await log.getByRole("button", { name: "Close" }).click();
    await expect(log).toBeHidden();

    const row = page.getByRole("listitem").filter({ hasText: folder });
    const problem = row.getByText("Claude Code's sign-in does not work for Remote Control.");
    await expect(problem).toBeVisible();
    expect(
      await problem.evaluate((note) => note.getBoundingClientRect().top),
    ).toBeGreaterThanOrEqual(
      await row
        .getByRole("group", { name: "Claude Code" })
        .evaluate((group) => group.getBoundingClientRect().bottom),
    );
    await row.getByRole("button", { name: `More ${folder} actions` }).click();
    await page.getByRole("menuitem", { name: "Show log" }).click();
    const folderLog = page.getByRole("dialog", { name: `${folder} log` });
    await expect(folderLog).toContainText(
      `/config/ezra/remote-control/folders/${folder}/server.log`,
    );
    await folderLog.getByRole("button", { name: "Close" }).click();

    await projects.getByRole("button", { name: "Sign in again" }).click();
    await expect(page.getByRole("heading", { name: "Sign in to Claude Code" })).toBeVisible();
  } finally {
    inContainer("rm", "-rf", `${PROJECTS_DIRECTORY}/${folder}`);
  }
});

test("a setting that stops Remote Control is named without offering a new sign-in", async ({
  page,
  request,
}) => {
  installFakeClaude("2.1.0-e2e", BLOCKED);
  await nudgeRemoteControl(request);
  await page.goto("./");
  const projects = projectsRow(page);
  await expect(
    projects.getByText("A Claude Code setting or environment variable stops Remote Control."),
  ).toBeVisible();
  await expect(projects.getByRole("button", { name: "Sign in again" })).toBeHidden();
});

test("a busy server waits for its sessions before restarting on an update", async ({
  page,
  request,
}) => {
  const server =
    "echo 'https://claude.ai/code?environment=env_e2e'; sh -c 'sleep 600; :' session --sdk-url & exec sleep 600";
  installFakeClaude("2.1.0-e2e", server);
  await nudgeRemoteControl(request);
  await page.goto("./");
  const projects = projectsRow(page);
  await expect(projects.getByText("Running", { exact: true })).toBeVisible();

  installFakeClaude("2.1.1-e2e", server);
  await nudgeRemoteControl(request);
  await expect(projects.getByText(/^Restarts on Claude Code 2\.1\.1-e2e by .+\.$/)).toBeVisible();
  await expect(projects.getByText("Running", { exact: true })).toBeVisible();
});

/** Writes Claude Code's credentials with a refresh token that stops working in `days`. */
function writeCredentials(days: number): void {
  const now = Date.now();
  writeInContainer(
    CLAUDE_CREDENTIALS,
    JSON.stringify({
      claudeAiOauth: {
        accessToken: "e2e",
        refreshToken: "e2e",
        expiresAt: now + 60 * 60 * 1000,
        refreshTokenExpiresAt: now + days * DAY_MS,
      },
    }),
    "600",
  );
}

test("the agents card warns days before Claude Code's sign-in ends until it is renewed", async ({
  page,
  request,
}) => {
  installFakeClaude("2.1.0-e2e", CONNECTED);
  writeCredentials(2);
  await nudgeRemoteControl(request);
  await page.goto("./");
  await expect(projectsRow(page).getByText("Running", { exact: true })).toBeVisible();
  const claude = page.getByRole("row", { name: /Claude Code/ });
  await expect(claude.getByText("Sign-in ending")).toBeVisible();
  await expect(claude.getByText(/^Sign-in ends /)).toBeVisible();
  await expect(claude.getByRole("button", { name: "Sign in again" })).toBeVisible();

  writeCredentials(30);
  await nudgeRemoteControl(request);
  await expect(claude.getByText("Sign-in ending")).toBeHidden();
  await expect(claude.getByRole("button", { name: "Sign in again" })).toBeHidden();
});

test("Claude Code's parts are hidden until it is installed", async ({
  page,
  request,
}, testInfo) => {
  const folder = `parts-${testInfo.retry}`;
  inContainer("mkdir", `${PROJECTS_DIRECTORY}/${folder}`);
  try {
    await page.goto("./settings");
    const settings = page.locator("section[data-slot=card]", {
      has: page.getByRole("heading", { name: "Claude Code", exact: true, level: 2 }),
    });
    const serveAll = settings.getByRole("switch", { name: "Serve to the Claude app" });
    await expect(settings.getByRole("radiogroup", { name: "Release channel" })).toBeVisible();
    await expect(serveAll).toBeHidden();

    await page.getByRole("link", { name: "Agents", exact: true }).click();
    const claude = page.getByRole("row", { name: /Claude Code/ });
    const row = page.getByRole("listitem").filter({ hasText: folder });
    const more = row.getByRole("button", { name: `More ${folder} actions` });
    const clone = page.getByRole("dialog", { name: "Clone repository" });
    const switchesNote = page.getByText(
      "The Claude app lists each folder switched on here, so sessions can start right in it.",
    );
    await expect(claude).toContainText("Waiting");
    await expect(row).toBeVisible();
    await expect(projectsRow(page)).toBeHidden();
    await expect(switchesNote).toBeHidden();
    await expect(row.getByRole("group", { name: "Claude Code" })).toBeHidden();
    await more.click();
    await expect(page.getByRole("menuitem", { name: "Delete" })).toBeVisible();
    await expect(page.getByRole("menuitem", { name: "Claude Code options" })).toBeHidden();
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Clone repository" }).click();
    await expect(clone.getByLabel("Folder")).toBeVisible();
    await expect(clone.getByRole("switch")).toBeHidden();
    await page.keyboard.press("Escape");

    installFakeClaude("2.1.0-e2e", CONNECTED);
    await nudgeRemoteControl(request);
    await expect(projectsRow(page).getByText("Running", { exact: true })).toBeVisible();
    await expect(switchesNote).toBeVisible();
    await expect(
      projectsRow(page).getByText(/^In the Claude app, pick .+ under Remote Control\.$/),
    ).toBeVisible();
    await expect(claude).toContainText("1 server, 0 sessions");
    await expect(
      row.getByRole("switch", { name: `Serve ${folder} in the Claude app` }),
    ).not.toBeChecked();
    await more.click();
    await expect(page.getByRole("menuitem", { name: "Claude Code options" })).toBeVisible();
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "Clone repository" }).click();
    await expect(clone.getByRole("switch", { name: "Serve in the Claude app" })).toBeVisible();
    await page.keyboard.press("Escape");

    await page.getByRole("link", { name: "Settings", exact: true }).click();
    await expect(serveAll).toBeVisible();
  } finally {
    inContainer("rmdir", `${PROJECTS_DIRECTORY}/${folder}`);
  }
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 360, height: 780 } });

  test("a folder's server problem sits under its Claude Code group", async ({
    page,
    request,
  }, testInfo) => {
    const folder = `problem-${testInfo.retry}`;
    installFakeClaude("2.1.0-e2e", REJECTED);
    await nudgeRemoteControl(request);
    inContainer("git", "init", "--quiet", `${PROJECTS_DIRECTORY}/${folder}`);
    try {
      await page.goto("./");
      const row = page.getByRole("listitem").filter({ hasText: folder });
      const problem = row.getByText("Claude Code's sign-in does not work for Remote Control.");
      await expect(problem).toBeVisible();
      expect(
        await problem.evaluate((note) => note.getBoundingClientRect().top),
      ).toBeGreaterThanOrEqual(
        await row
          .getByRole("group", { name: "Claude Code" })
          .evaluate((group) => group.getBoundingClientRect().bottom),
      );
    } finally {
      inContainer("rm", "-rf", `${PROJECTS_DIRECTORY}/${folder}`);
    }
  });
});
