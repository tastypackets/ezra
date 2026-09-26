import { expect, test } from "@playwright/test";
import type { APIRequestContext, Page } from "@playwright/test";

import { inContainer, writeInContainer } from "./manager.ts";

const VERSIONS = "/home/dev/.local/share/claude/versions";
const COMMAND = "/home/dev/.local/bin/claude";
const CREDENTIALS = "/config/claude/.credentials.json";
const DAY_MS = 86_400_000;

/** Links a `claude` script as `version` whose Remote Control runs `server`. */
function installFakeClaude(version: string, server: string): void {
  writeInContainer(
    `${VERSIONS}/${version}`,
    `#!/bin/sh
case "$1 $2" in
  "auth status") echo '{"loggedIn":true,"email":"e2e@example.com","subscriptionType":"max"}' ;;
  "auth login") echo 'Visit https://claude.ai/oauth/authorize?code=true to sign in'; exec sleep 300 ;;
  remote-control*) ${server} ;;
esac
`,
    "755",
  );
  inContainer("mkdir", "-p", "/home/dev/.local/bin");
  inContainer("ln", "-sfn", `${VERSIONS}/${version}`, COMMAND);
}

/** A settings change makes the manager check Claude Code's sign-in and servers at once. */
async function nudgeRemoteControl(request: APIRequestContext): Promise<void> {
  const settings = await (await request.get("api/v1/agents/claude/settings")).json();
  for (const serve_repositories of [!settings.remote_control.serve_repositories, true]) {
    const saved = await request.put("api/v1/agents/claude/settings", {
      data: { ...settings, remote_control: { ...settings.remote_control, serve_repositories } },
    });
    expect(saved.ok()).toBe(true);
  }
}

async function removeFakeClaude(request: APIRequestContext): Promise<void> {
  await request.post("api/v1/agents/claude/logout");
  inContainer("rm", "-rf", COMMAND, VERSIONS, CREDENTIALS);
  await nudgeRemoteControl(request);
}

function remoteControlCard(page: Page) {
  return page.locator("section[data-slot=card]", {
    has: page.getByRole("heading", { name: /^Remote Control/, level: 2 }),
  });
}

test.afterEach(async ({ request }) => {
  await removeFakeClaude(request);
});

test("a rejected sign-in is explained, logged and fixed by signing in again", async ({
  page,
  request,
}, testInfo) => {
  const folder = `rejected-${testInfo.retry}`;
  installFakeClaude(
    "2.1.0-e2e",
    "echo 'Error: You must be logged in to use Remote Control.' >&2; exit 1",
  );
  await nudgeRemoteControl(request);
  inContainer("git", "init", "--quiet", `/projects/${folder}`);
  try {
    await page.goto("./");
    const card = remoteControlCard(page);
    await expect(
      card.getByText("Claude Code's sign-in does not work for Remote Control."),
    ).toBeVisible();

    await card.getByRole("button", { name: "More Remote Control actions" }).click();
    await page.getByRole("menuitem", { name: "Show log" }).click();
    const log = page.getByRole("dialog", { name: "/projects log" });
    await expect(log).toContainText("/config/ezra/remote-control/projects/server.log");
    await expect(log.getByLabel("Log lines")).toContainText(
      "[OUTPUT] Error: You must be logged in to use Remote Control.",
    );
    await log.getByRole("button", { name: "Close" }).click();
    await expect(log).toBeHidden();

    const row = page.getByRole("listitem").filter({ hasText: folder });
    await expect(
      row.getByText("Claude Code's sign-in does not work for Remote Control."),
    ).toBeVisible();
    await row.getByRole("button", { name: `More actions for ${folder}` }).click();
    await page.getByRole("menuitem", { name: "Show log" }).click();
    const folderLog = page.getByRole("dialog", { name: `${folder} log` });
    await expect(folderLog).toContainText(
      `/config/ezra/remote-control/folders/${folder}/server.log`,
    );
    await folderLog.getByRole("button", { name: "Close" }).click();

    await card.getByRole("button", { name: "Sign in again" }).click();
    await expect(page.getByRole("heading", { name: "Sign in to Claude Code" })).toBeVisible();
  } finally {
    inContainer("rm", "-rf", `/projects/${folder}`);
  }
});

test("a busy server waits for its sessions before restarting on an update", async ({
  page,
  request,
}) => {
  const server = "echo 'https://claude.ai/code?environment=env_e2e'; sleep 600 & exec sleep 600";
  installFakeClaude("2.1.0-e2e", server);
  await nudgeRemoteControl(request);
  await page.goto("./");
  const card = remoteControlCard(page);
  await expect(card.getByText("Running", { exact: true })).toBeVisible();

  installFakeClaude("2.1.1-e2e", server);
  await nudgeRemoteControl(request);
  await expect(
    card.getByText(
      /^Restarts on Claude Code 2\.1\.1-e2e once no sessions run, by .+ at the latest\.$/,
    ),
  ).toBeVisible();
  await expect(card.getByText("Running", { exact: true })).toBeVisible();
});

/** Writes Claude Code's credentials with a refresh token that stops working in `days`. */
function writeCredentials(days: number): void {
  const now = Date.now();
  writeInContainer(
    CREDENTIALS,
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
  installFakeClaude(
    "2.1.0-e2e",
    "echo 'https://claude.ai/code?environment=env_e2e'; exec sleep 600",
  );
  writeCredentials(2);
  await nudgeRemoteControl(request);
  await page.goto("./");
  await expect(remoteControlCard(page).getByText("Running", { exact: true })).toBeVisible();
  const claude = page.getByRole("row", { name: /Claude Code/ });
  await expect(claude.getByText("Sign-in ending")).toBeVisible();
  await expect(claude.getByText(/^Sign-in ends /)).toBeVisible();
  await expect(claude.getByRole("button", { name: "Sign in again" })).toBeVisible();

  writeCredentials(30);
  await nudgeRemoteControl(request);
  await expect(claude.getByText("Sign-in ending")).toBeHidden();
  await expect(claude.getByRole("button", { name: "Sign in again" })).toBeHidden();
});
