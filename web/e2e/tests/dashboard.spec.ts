import { expect, test } from "@playwright/test";

import { inContainer, recordApiCalls } from "./manager.ts";

test("the dashboard arrives with its data, then only listens for changes", async ({ page }) => {
  const apiCalls = recordApiCalls(page);
  const listening = page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/api/v1/events",
  );
  await page.goto("./");
  for (const agent of ["Claude Code", "Codex"]) {
    const row = page.getByRole("row", { name: new RegExp(agent) });
    await expect(row).toContainText("Not installed");
    await expect(row.getByRole("button", { name: "Install" })).toBeVisible();
  }
  await expect(
    page.getByText("No projects yet. Ask an agent to clone a repository into /projects."),
  ).toBeVisible();
  await listening;
  expect(apiCalls).toEqual(["GET /api/v1/events"]);
});

test("a folder's switch and server follow changes made elsewhere", async ({ page }, testInfo) => {
  const folder = `notes-${testInfo.retry}`;
  inContainer("mkdir", `/projects/${folder}`);
  try {
    await page.goto("./");
    const row = page.getByRole("listitem").filter({ hasText: folder });
    const serve = row.getByRole("switch", { name: `Serve ${folder} in the Claude app` });
    await expect(serve).not.toBeChecked();
    const chosen = await page.request.put(`api/v1/folders/${folder}/remote-control`, {
      data: { serve: true },
    });
    expect(chosen.status()).toBe(204);
    await expect(serve).toBeChecked();
    await expect(row.getByText("Waiting")).toBeVisible();
    await serve.click();
    await expect(serve).not.toBeChecked();
    await expect(row.getByText("Waiting")).toBeHidden();
  } finally {
    inContainer("rmdir", `/projects/${folder}`);
  }
});

test("Remote Control waits for Claude Code", async ({ page }) => {
  await page.goto("./");
  await expect(page.getByText("Starts once Claude Code is installed and signed in.")).toBeVisible();
});

test("hovering Settings loads its data before the click", async ({ page }) => {
  await page.goto("./");
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
  const settingsLoaded = page.waitForRequest(
    (request) => new URL(request.url()).pathname === "/api/v1/agents/claude/settings",
  );
  await page.getByRole("link", { name: "Settings", exact: true }).hover();
  await settingsLoaded;
});

test("signed in, the sign-in page goes to the dashboard", async ({ page }) => {
  await page.goto("./login");
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
});

test("an ended session returns to sign in", async ({ page }) => {
  await page.goto("./");
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
  await page.context().clearCookies();
  await page
    .getByRole("row", { name: /Claude Code/ })
    .getByRole("button", { name: "Install" })
    .click();
  await expect(page).toHaveURL(/\/login$/);
  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
});

test("an unknown page links back to the dashboard", async ({ page }) => {
  await page.goto("./nowhere");
  await expect(page.getByRole("heading", { name: "Page not found" })).toBeVisible();
  await page.getByRole("link", { name: "Go to the dashboard" }).click();
  await expect(page.getByRole("heading", { name: "Agents" })).toBeVisible();
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 360, height: 780 } });

  test("the dashboard fits the screen", async ({ page }) => {
    await page.goto("./");
    const claude = page.getByRole("listitem").filter({ hasText: "Claude Code" });
    await expect(claude.getByRole("button", { name: "Install" })).toBeInViewport();
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBe(0);
  });
});
