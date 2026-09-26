import { expect, test } from "@playwright/test";

import {
  inContainer,
  installFakeClaude,
  nudgeRemoteControl,
  recordApiCalls,
  removeFakeClaude,
} from "./manager.ts";

test("the dashboard arrives with its data, then fetches only what changes", async ({
  page,
}, testInfo) => {
  const apiCalls = recordApiCalls(page);
  const listening = page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/api/v1/events",
  );
  await page.goto("./");
  for (const agent of ["Claude Code", "Codex"]) {
    const row = page.getByRole("row", { name: new RegExp(agent) });
    await expect(row.getByRole("rowheader", { name: agent })).toBeVisible();
    await expect(row).toContainText("Not installed");
    await expect(row.getByRole("button", { name: "Install" })).toHaveAccessibleDescription(agent);
  }
  await expect(page.getByText("No projects yet.")).toBeVisible();
  await listening;

  const folder = `added-${testInfo.retry}`;
  const refetched = page.waitForResponse(
    (response) => new URL(response.url()).pathname === "/api/v1/folders",
  );
  inContainer("mkdir", `/projects/${folder}`);
  try {
    await refetched;
    await expect(page.getByRole("listitem").filter({ hasText: folder })).toBeVisible();
    expect(apiCalls).toEqual(["GET /api/v1/events", "GET /api/v1/folders"]);
  } finally {
    inContainer("rmdir", `/projects/${folder}`);
  }
});

test("keyboard focus stays in an agent's row", async ({ page }) => {
  // Installing and signing in reach the vendors, so the manager's replies are stubbed.
  const codex = { agent: "codex", configured: false, logged_in: false };
  const installed = {
    agent: "claude",
    configured: true,
    logged_in: false,
    installed_version: "2.1.283",
  };
  const prompt = { url: "https://claude.com/cai/oauth/authorize" };
  let claude: object = { agent: "claude", configured: false, logged_in: false };
  await page.route("**/api/v1/agents", (route) => route.fulfill({ json: [claude, codex] }));
  await page.route("**/api/v1/agents/claude/install", (route) => {
    claude =
      "logged_in" in claude && claude.logged_in
        ? { ...installed, logged_in: true, installed_version: "2.1.284" }
        : installed;
    return route.fulfill({ json: claude });
  });
  await page.route("**/api/v1/agents/claude/login", (route) => {
    claude = { ...installed, login_prompt: prompt };
    return route.fulfill({ json: prompt });
  });
  await page.route("**/api/v1/agents/claude/login/code", (route) => {
    claude = { ...installed, logged_in: true, available_update: "2.1.284" };
    return route.fulfill({ status: 204 });
  });

  await page.goto("./");
  const row = page.getByRole("row", { name: /Claude Code/ });
  await row.getByRole("button", { name: "Install" }).focus();
  await page.keyboard.press("Enter");
  const signIn = row.getByRole("button", { name: "Sign in" });
  await expect(signIn).toBeFocused();
  await expect(signIn).toHaveAccessibleDescription("Claude Code");

  const more = row.getByRole("button", { name: "More Claude Code actions" });
  await page.keyboard.press("Tab");
  await expect(more).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("menuitem", { name: "Check for update" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Claude Code is up to date.")).toBeVisible();
  await expect(more).toBeFocused();

  await page.keyboard.press("Shift+Tab");
  await expect(signIn).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "Sign in to Claude Code" })).toBeFocused();
  await page.getByRole("textbox", { name: "Code" }).fill("code");
  await page.keyboard.press("Enter");

  const update = row.getByRole("button", { name: "Update to 2.1.284" });
  await expect(update).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(update).toBeHidden();
  await expect(more).toBeFocused();
});

test.describe("with Claude Code installed", () => {
  test.beforeEach(() => installFakeClaude("2.1.0-e2e"));
  test.afterEach(({ request }) => removeFakeClaude(request));

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

  test("a served folder's link names the folder", async ({ page }, testInfo) => {
    const folder = `linked-${testInfo.retry}`;
    const url = "https://claude.ai/code?environment=env_e2e";
    // A running server needs a signed-in Claude Code, so the manager's reply is stubbed.
    await page.route("**/api/v1/remote-control", async (route) => {
      const overview: { folders: Record<string, object> } = await (await route.fetch()).json();
      overview.folders[folder] = { state: "running", restarts: 0, url };
      await route.fulfill({ json: overview });
    });
    inContainer("mkdir", `/projects/${folder}`);
    try {
      await page.goto("./");
      const row = page.getByRole("listitem").filter({ hasText: folder });
      const serve = row.getByRole("switch", { name: `Serve ${folder} in the Claude app` });
      await serve.click();
      await expect(
        row.getByRole("link", { name: `Open ${folder} on claude.ai/code` }),
      ).toHaveAttribute("href", url);
      await serve.click();
      await expect(serve).not.toBeChecked();
    } finally {
      inContainer("rmdir", `/projects/${folder}`);
    }
  });

  test("a new repository starts with its switch on", async ({ page }, testInfo) => {
    const folder = `repository-${testInfo.retry}`;
    await page.goto("./");
    await expect(page.getByRole("heading", { name: "Folders" })).toBeVisible();
    inContainer("git", "init", "--quiet", `/projects/${folder}`);
    try {
      const row = page.getByRole("listitem").filter({ hasText: folder });
      await expect(
        row.getByRole("switch", { name: `Serve ${folder} in the Claude app` }),
      ).toBeChecked();
      await expect(row.getByText("Waiting")).toBeVisible();
    } finally {
      inContainer("rm", "-rf", `/projects/${folder}`);
    }
  });

  test("Remote Control waits for Claude Code's sign-in", async ({ page }) => {
    await page.goto("./");
    const projects = page.getByRole("listitem").filter({ hasText: "All projects" });
    await expect(projects.getByText("Waiting")).toBeVisible();
    await expect(projects.getByText("Starts once Claude Code is signed in.")).toBeVisible();
    await expect(page.getByRole("row", { name: /Claude Code/ })).toContainText("Waiting");
  });
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

test.describe("in a narrow window", () => {
  test.use({ viewport: { width: 800, height: 780 } });

  test("a long repository gives way to the Claude Code group", async ({
    page,
    request,
  }, testInfo) => {
    const folder = `long-${testInfo.retry}`;
    const repository =
      "https://github.com/a-fairly-long-organization-name/some-rather-long-repository-name.git";
    installFakeClaude(
      "2.1.0-e2e",
      "echo 'https://claude.ai/code?environment=env_e2e'; exec sleep 600",
    );
    await nudgeRemoteControl(request);
    inContainer(
      "sh",
      "-c",
      'git init --quiet --initial-branch=feature/a-long-branch-name-for-testing "$1" && git -C "$1" remote add origin "$2"',
      "sh",
      `/projects/${folder}`,
      repository,
    );
    try {
      await page.goto("./");
      const row = page.getByRole("listitem").filter({ hasText: folder });
      const group = row.getByRole("group", { name: "Claude Code" });
      const state = group.getByText("Running", { exact: true });
      await expect(group.getByText("0 sessions")).toBeVisible();
      await expect(state).toBeVisible();
      await expect(row.getByText(repository)).toBeVisible();
      const middle = (element: Element) => {
        const box = element.getBoundingClientRect();
        return box.top + box.height / 2;
      };
      expect(await group.getByRole("switch").evaluate(middle)).toBeCloseTo(
        await state.evaluate(middle),
        0,
      );
      expect(
        await row
          .getByText(repository)
          .evaluate((element) => element.scrollWidth > element.clientWidth),
      ).toBe(true);
    } finally {
      inContainer("rm", "-rf", `/projects/${folder}`);
      await removeFakeClaude(request);
    }
  });
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 360, height: 780 } });

  test("the dashboard fits the screen", async ({ page }) => {
    await page.goto("./");
    const claude = page.getByRole("listitem").filter({ hasText: "Claude Code" });
    const install = claude.getByRole("button", { name: "Install" });
    await expect(install).toBeInViewport();
    await expect(install).toHaveAccessibleDescription("Claude Code");
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBe(0);
  });

  test("a served folder fits the screen", async ({ page, request }, testInfo) => {
    const folder = `phone-${testInfo.retry}`;
    installFakeClaude(
      "2.1.0-e2e",
      "echo 'https://claude.ai/code?environment=env_e2e'; exec sleep 600",
    );
    await nudgeRemoteControl(request);
    inContainer("git", "init", "--quiet", `/projects/${folder}`);
    try {
      await page.goto("./");
      const row = page.getByRole("listitem").filter({ hasText: folder });
      await expect(row.getByText("Running", { exact: true })).toBeVisible();
      await row.scrollIntoViewIfNeeded();
      await expect(
        row.getByRole("switch", { name: `Serve ${folder} in the Claude app` }),
      ).toBeInViewport({ ratio: 1 });
      await expect(row.getByRole("button", { name: `More ${folder} actions` })).toBeInViewport({
        ratio: 1,
      });
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - window.innerWidth,
      );
      expect(overflow).toBe(0);
    } finally {
      inContainer("rm", "-rf", `/projects/${folder}`);
      await removeFakeClaude(request);
    }
  });
});
