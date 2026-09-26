import { expect, test } from "@playwright/test";
import type { Page, PlaywrightTestOptions, PlaywrightWorkerArgs } from "@playwright/test";

import { PASSWORD } from "./manager.ts";

function card(page: Page, title: string) {
  return page.locator("section[data-slot=card]", {
    has: page.getByRole("heading", { name: title, exact: true, level: 2 }),
  });
}

test("the Claude release channel is saved", async ({ page }) => {
  await page.goto("./");
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  const claude = card(page, "Claude Code");
  const latest = claude.getByRole("radio", { name: "Latest" });
  const stable = claude.getByRole("radio", { name: "Stable" });
  await expect(latest).toBeChecked();

  await stable.click();
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();

  await page.reload();
  await expect(stable).toBeChecked();
  await latest.click();
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();
});

test("only the newest three toasts show", async ({ page }) => {
  await page.goto("./settings");
  const save = card(page, "Claude Code").getByRole("button", { name: "Save" });
  const saved = page.getByText("Claude Code settings saved.");
  for (const count of [1, 2, 3, 4]) {
    await save.click();
    await expect(saved).toHaveCount(count);
  }
  await expect(saved.filter({ visible: true })).toHaveCount(3);
});

test("every Claude setting is named and described", async ({ page }) => {
  await page.goto("./settings");
  const claude = card(page, "Claude Code");
  await expect(
    claude.getByRole("switch", { name: "Serve to the Claude app" }),
  ).toHaveAccessibleDescription(
    "Serves /projects, and the folders you choose, while Claude Code is signed in.",
  );
  await expect(
    claude.getByRole("switch", { name: "Serve new repositories" }),
  ).toHaveAccessibleDescription("Repositories added to /projects start with their switch on.");
  await expect(
    claude.getByRole("combobox", { name: "Permission mode" }),
  ).toHaveAccessibleDescription("Sessions started from the Claude app keep this mode.");
  await expect(
    claude.getByRole("radiogroup", { name: "Release channel" }),
  ).toHaveAccessibleDescription(
    "Switching to stable keeps the installed version until stable has a newer one.",
  );
  await expect(claude.getByRole("radio", { name: "Stable" })).toHaveAccessibleDescription(
    "About a week behind, skipping releases with major regressions.",
  );
});

test("Remote Control settings are saved and shown on the dashboard", async ({ page }) => {
  await page.goto("./settings");
  const claude = card(page, "Claude Code");
  const serve = claude.getByRole("switch", { name: "Serve to the Claude app" });
  const mode = page.getByRole("combobox", { name: "Permission mode" });
  const capacity = claude.getByLabel("Sessions per folder");
  await expect(capacity).toHaveValue("");
  await expect(capacity).toHaveAttribute("placeholder", "Claude Code's default");

  await serve.setChecked(false);
  await mode.fill("plan");
  await mode.press("Enter");
  await expect(mode).toHaveValue("plan");
  await mode.fill("acceptEdits");
  await page.keyboard.press("Escape");
  await capacity.fill("2");
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();

  await page.reload();
  await expect(serve).not.toBeChecked();
  await expect(mode).toHaveValue("acceptEdits");
  await expect(capacity).toHaveValue("2");
  await page.getByRole("link", { name: "Agents", exact: true }).click();
  await expect(page.getByRole("link", { name: "Turned off in Settings." })).toBeVisible();

  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await serve.setChecked(true);
  await mode.fill("auto");
  await page.keyboard.press("Escape");
  await capacity.fill("");
  await claude.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();
});

test("a setting Claude cannot take is refused next to its field", async ({ page }) => {
  await page.goto("./settings");
  const claude = card(page, "Claude Code");
  const capacity = claude.getByRole("spinbutton", { name: "Sessions per folder" });
  await capacity.fill("40");
  await expect(claude.getByText("Enter a number from 1 to 32, or leave it empty.")).toBeVisible();
  await expect(capacity).toHaveAccessibleDescription(
    "Each session is its own Claude Code process, about 150 to 300 MB. Enter a number from 1 to 32, or leave it empty.",
  );
  await expect(capacity).toHaveAttribute("aria-invalid", "true");

  const mode = claude.getByRole("combobox", { name: "Permission mode" });
  await mode.fill("two words");
  await page.keyboard.press("Escape");
  await expect(mode).toHaveAccessibleDescription(
    "Sessions started from the Claude app keep this mode. Enter one word, such as auto.",
  );
});

test("git starts signed out of GitHub and keeps the commit identity", async ({ page }) => {
  await page.goto("./settings");
  const git = card(page, "Git");
  await expect(git.getByText("Signed out")).toBeVisible();
  await expect(git.getByRole("button", { name: "Sign in to GitHub" })).toBeVisible();

  await git.getByLabel("Name").fill("Ada Lovelace");
  await git.getByLabel("Email").fill("ada@example.com");
  await git.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("Commit identity saved.")).toBeVisible();
  const footerPadding = await git.evaluate((section) => {
    const save = section.querySelector("button[type=submit]");
    return section.getBoundingClientRect().bottom - (save?.getBoundingClientRect().bottom ?? 0);
  });
  expect(footerPadding).toBe(16);

  await page.reload();
  await expect(git.getByLabel("Name")).toHaveValue("Ada Lovelace");
  await expect(git.getByLabel("Email")).toHaveValue("ada@example.com");
});

async function signInElsewhere(
  playwright: PlaywrightWorkerArgs["playwright"],
  baseURL: PlaywrightTestOptions["baseURL"],
) {
  const elsewhere = await playwright.request.newContext({ baseURL, ignoreHTTPSErrors: true });
  const signedIn = await elsewhere.post("api/v1/login", { data: { password: PASSWORD } });
  expect(signedIn.status()).toBe(204);
  return elsewhere;
}

test("changing the password needs the current one and signs out other sessions", async ({
  page,
  playwright,
  baseURL,
}) => {
  await page.goto("./settings");
  const manager = card(page, "Manager");
  const current = manager.getByLabel("Current password");
  const next = manager.getByLabel("New password");
  const change = manager.getByRole("button", { name: "Change password" });

  await current.fill("not the password");
  await next.fill("battery staple");
  await change.click();
  await expect(manager.getByText("the current password is wrong")).toBeVisible();
  await expect(current).toHaveAccessibleDescription("the current password is wrong");

  const elsewhere = await signInElsewhere(playwright, baseURL);
  try {
    await current.fill(PASSWORD);
    await next.fill("battery staple");
    await change.click();
    await expect(
      page.getByText("Password changed, other sessions signed out.").last(),
    ).toBeVisible();
    expect((await elsewhere.get("api/v1/manager")).status()).toBe(401);
  } finally {
    await elsewhere.dispose();
  }

  await current.fill("battery staple");
  await next.fill(PASSWORD);
  await change.click();
  await expect(page.getByText("Password changed, other sessions signed out.").last()).toBeVisible();
});

test("the certificate can be regenerated", async ({ page }) => {
  await page.goto("./settings");
  const manager = card(page, "Manager");
  await expect(manager.getByText(/^Expires /)).toBeVisible();

  await manager.getByRole("button", { name: "Regenerate" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Regenerate the certificate?" });
  await expect(dialog).toContainText("Browsers warn again until you accept the new certificate.");
  await dialog.getByRole("button", { name: "Regenerate certificate" }).click();
  await expect(page.getByText("Certificate regenerated.").last()).toBeVisible();
});

test("the environment is shown read-only", async ({ page }) => {
  await page.goto("./settings");
  const environment = card(page, "Environment");
  await expect(environment.getByText("EZRA_PORT")).toBeVisible();
  await expect(environment.getByText("8443")).toBeVisible();
  await expect(environment.getByRole("textbox")).toHaveCount(0);
});

test("GitHub sign-in steps take focus and hand it back when they close", async ({ page }) => {
  // Starting a GitHub sign-in asks github.com for a code, so the manager's replies are stubbed.
  const prompt = { url: "https://github.com/login/device", code: "ABCD-1234" };
  let github: object = { failing: false, from_environment: false, signed_in: false };
  await page.route("**/api/v1/git", async (route) => {
    const status: object = await (await route.fetch()).json();
    await route.fulfill({ json: { ...status, github } });
  });
  await page.route("**/api/v1/git/github/login", (route) => {
    github = { ...github, login_prompt: prompt };
    return route.fulfill({ json: prompt });
  });

  await page.goto("./settings");
  const box = card(page, "Git");
  await box.getByRole("button", { name: "Sign in to GitHub" }).focus();
  await page.keyboard.press("Enter");
  const steps = box.getByRole("group", { name: "Sign in to GitHub" });
  await expect(steps).toBeFocused();
  await expect(box.getByText("ABCD-1234")).toBeVisible();

  github = { failing: false, from_environment: false, signed_in: true, account: "octocat" };
  await expect(steps).toBeHidden();
  await expect(box.getByRole("button", { name: "Sign out" })).toBeFocused();
});
