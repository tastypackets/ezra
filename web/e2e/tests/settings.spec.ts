import { expect, test } from "@playwright/test";
import type { Page, PlaywrightTestOptions, PlaywrightWorkerArgs, Route } from "@playwright/test";

import {
  CODEX_SIGNED_OUT,
  PASSWORD,
  installFakeClaude,
  installFakeCodex,
  removeFakeClaude,
  removeFakeCodex,
} from "./manager.ts";

const CODEX_DEFAULTS = {
  remote_control: { enabled: true, sandbox: "danger-full-access", approvals: "on-request" },
};

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
  await expect(page.locator("[data-slot=toast]:not([data-limited])")).toHaveCount(3);
});

test.describe("with Claude Code installed", () => {
  test.beforeEach(() => installFakeClaude("2.1.0-e2e"));
  test.afterEach(({ request }) => removeFakeClaude(request));

  test("every Claude setting is named and described", async ({ page }) => {
    await page.goto("./settings");
    const claude = card(page, "Claude Code");
    await expect(
      claude.getByRole("switch", { name: "Serve to the Claude app" }),
    ).toHaveAccessibleDescription(
      "Serves ~/projects, and the folders you choose, while Claude Code is signed in.",
    );
    await expect(
      claude.getByRole("switch", { name: "Serve new repositories" }),
    ).toHaveAccessibleDescription("Repositories added to ~/projects start with their switch on.");
    await expect(
      claude.getByRole("radiogroup", { name: "New sessions in repositories work in" }),
    ).toHaveAccessibleDescription("A folder's Claude Code options can choose otherwise.");
    await expect(claude.getByRole("radio", { name: "Their own worktree" })).toBeChecked();
    await expect(
      claude.getByRole("combobox", { name: "Permission mode" }),
    ).toHaveAccessibleDescription("New sessions from the Claude app start in this mode.");
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

    const inFolder = claude.getByRole("radio", { name: "The folder" });
    await serve.setChecked(false);
    await inFolder.click();
    await mode.fill("plan");
    await mode.press("Enter");
    await expect(mode).toHaveValue("plan");
    await mode.fill("acceptEdits");
    await page.keyboard.press("Escape");
    await capacity.fill("2");
    await claude.locator("form").getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();

    await page.reload();
    await expect(serve).not.toBeChecked();
    await expect(inFolder).toBeChecked();
    await expect(mode).toHaveValue("acceptEdits");
    await expect(capacity).toHaveValue("2");
    await page.getByRole("link", { name: "Agents", exact: true }).click();
    await expect(page.getByRole("link", { name: "Turned off in Settings." })).toBeVisible();

    await page.getByRole("link", { name: "Settings", exact: true }).click();
    await serve.setChecked(true);
    await claude.getByRole("radio", { name: "Their own worktree" }).click();
    await mode.fill("auto");
    await page.keyboard.press("Escape");
    await capacity.fill("");
    await claude.locator("form").getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Claude Code settings saved.").last()).toBeVisible();
  });

  test("a setting Claude cannot take is refused next to its field", async ({ page }) => {
    await page.goto("./settings");
    const claude = card(page, "Claude Code");
    const capacity = claude.getByRole("spinbutton", { name: "Sessions per folder" });
    await capacity.fill("0");
    await expect(
      claude.getByText("Enter a whole number of 1 or more, or leave it empty."),
    ).toBeVisible();
    await expect(capacity).toHaveAccessibleDescription(
      "Each session is its own Claude Code process. Enter a whole number of 1 or more, or leave it empty.",
    );
    await expect(capacity).toHaveAttribute("aria-invalid", "true");

    const mode = claude.getByRole("combobox", { name: "Permission mode" });
    await mode.fill("two words");
    await page.keyboard.press("Escape");
    await expect(mode).toHaveAccessibleDescription(
      "New sessions from the Claude app start in this mode. Choose one of the listed modes.",
    );
  });
});

test.describe("with Codex installed", () => {
  test.beforeEach(() => installFakeCodex("9.9.9-settings", CODEX_SIGNED_OUT));
  test.afterEach(async ({ request }) => {
    try {
      const reset = await request.put("api/v1/agents/codex/settings", { data: CODEX_DEFAULTS });
      expect(reset.ok()).toBe(true);
    } finally {
      await removeFakeCodex(request);
    }
  });

  test("every Codex setting is named and described", async ({ page }) => {
    await page.goto("./settings");
    const codex = card(page, "Codex");
    await expect(
      codex.getByRole("switch", { name: "Serve to the ChatGPT app" }),
    ).toHaveAccessibleDescription("Serves this box while Codex is signed in with ChatGPT.");
    await expect(codex.getByRole("radiogroup", { name: "Sandbox" })).toHaveAccessibleDescription(
      "Read only and Workspace write need a container that allows user namespaces.",
    );
    await expect(codex.getByRole("radiogroup", { name: "Approvals" })).toHaveAccessibleDescription(
      "Codex's questions go to the ChatGPT app.",
    );
    const options = [
      ["No sandbox", "The container is the only boundary."],
      [
        "Workspace write",
        "Commands can write in the chat's folder, except .git, and in /tmp, with no network.",
      ],
      ["Read only", "Commands can read files but not change them."],
      ["On request", "Codex asks when it needs to, such as before rm -\u2060rf."],
      ["Never", "Codex never asks and refuses commands like rm -\u2060rf."],
    ] as const;
    for (const [name, description] of options) {
      await expect(codex.getByRole("radio", { name, exact: true })).toHaveAccessibleDescription(
        description,
      );
    }
  });

  test("Codex settings start at their defaults and are saved", async ({ page }) => {
    await page.goto("./settings");
    const codex = card(page, "Codex");
    const serve = codex.getByRole("switch", { name: "Serve to the ChatGPT app" });
    const readOnly = codex.getByRole("radio", { name: "Read only", exact: true });
    const never = codex.getByRole("radio", { name: "Never", exact: true });
    await expect(serve).toBeChecked();
    await expect(codex.getByRole("radio", { name: "No sandbox", exact: true })).toBeChecked();
    await expect(codex.getByRole("radio", { name: "On request", exact: true })).toBeChecked();

    await serve.setChecked(false);
    await readOnly.click();
    await never.click();
    await codex.locator("form").getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Codex settings saved.").last()).toBeVisible();

    await page.reload();
    await expect(serve).not.toBeChecked();
    await expect(readOnly).toBeChecked();
    await expect(never).toBeChecked();
  });

  test("a failed Codex save shows why and keeps the choices", async ({ page }) => {
    await page.route("**/api/v1/agents/codex/settings", (route) =>
      route.request().method() === "PUT"
        ? route.fulfill({ status: 500, json: { error: "could not write settings" } })
        : route.fallback(),
    );
    await page.goto("./settings");
    const codex = card(page, "Codex");
    const never = codex.getByRole("radio", { name: "Never", exact: true });
    await never.click();
    await codex.locator("form").getByRole("button", { name: "Save" }).click();
    await expect(codex.locator("form").getByRole("alert")).toHaveText("could not write settings");
    await expect(never).toBeChecked();
    await expect(page.getByText("Codex settings saved.")).toHaveCount(0);
  });
});

test("the Codex card is hidden until Codex is installed", async ({ page, request }) => {
  await page.goto("./settings");
  await expect(card(page, "Claude Code")).toBeVisible();
  await expect(card(page, "Codex")).toBeHidden();

  installFakeCodex("9.9.9-settings", CODEX_SIGNED_OUT);
  try {
    await page.reload();
    await expect(
      card(page, "Codex").getByRole("switch", { name: "Serve to the ChatGPT app" }),
    ).toBeVisible();
  } finally {
    await removeFakeCodex(request);
  }
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
  await expect(environment.getByText("GH_HOST")).toBeVisible();
  await expect(environment.getByText("GH_TOKEN or GITHUB_TOKEN")).toBeVisible();
  await expect(environment.getByRole("textbox")).toHaveCount(0);
});

test("an Enterprise host signed in by its token variable is named on the Git card", async ({
  page,
}) => {
  const github = {
    host: "ghe.example.com",
    signed_in: true,
    account: "octocat",
    from_environment: true,
    token_variables: ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"],
  };
  await page.route("**/api/v1/git", async (route) => {
    const status: { github: object } = await (await route.fetch()).json();
    await route.fulfill({ json: { ...status, github: { ...status.github, ...github } } });
  });

  await page.goto("./settings");
  const git = card(page, "Git");
  await expect(git.getByRole("heading", { name: "ghe.example.com" })).toBeVisible();
  await expect(
    git.getByText("Set by the GH_ENTERPRISE_TOKEN or GITHUB_ENTERPRISE_TOKEN variable."),
  ).toBeVisible();
  await expect(git.getByRole("button", { name: "Sign out" })).toHaveCount(0);
});

test("GitHub sign-in steps take focus and hand it back when they close", async ({ page }) => {
  // Starting a GitHub sign-in asks github.com for a code, so the manager's replies are stubbed.
  const prompt = { url: "https://github.com/login/device", code: "ABCD-1234" };
  let github: object = { failing: false, from_environment: false, signed_in: false };
  await page.route("**/api/v1/git", async (route) => {
    const status: { github: object } = await (await route.fetch()).json();
    await route.fulfill({ json: { ...status, github: { ...status.github, ...github } } });
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

test("GitHub trigger settings persist and reject duplicate shortcuts", async ({
  page,
  request,
}) => {
  const path = "/api/v1/inbound/settings";
  try {
    await page.goto("./settings");
    const triggers = card(page, "GitHub triggers");
    await expect(
      triggers.getByRole("switch", { name: "Only repositories added to Ezra" }),
    ).toBeChecked();
    await triggers.getByRole("switch", { name: "Only repositories added to Ezra" }).uncheck();
    await triggers.getByLabel("Command", { exact: true }).fill("/ezra-fast");
    await triggers.getByLabel("Chat model", { exact: true }).fill("future-model");
    await triggers.getByLabel("Chat effort", { exact: true }).fill("high");
    await triggers.getByLabel("History retention in days").fill("120");
    await expect(triggers.getByLabel("Waiting request expiry in hours")).toHaveValue("24");
    await triggers.getByLabel("Waiting request expiry in hours").fill("48");
    await expect(triggers.getByLabel("Polling interval in seconds")).toHaveValue("30");
    await triggers.getByLabel("Polling interval in seconds").fill("10");
    await triggers.getByRole("radio", { name: "Edit a status footer into my comment" }).check();
    await triggers.getByRole("button", { name: "Save", exact: true }).click();
    await expect(
      page.getByText("GitHub trigger settings saved", { exact: true }).last(),
    ).toBeVisible();
    await page.reload();

    await expect(
      triggers.getByRole("switch", { name: "Only repositories added to Ezra" }),
    ).not.toBeChecked();
    await expect(triggers.getByLabel("Command", { exact: true })).toHaveValue("/ezra-fast");
    await expect(triggers.getByLabel("Chat effort", { exact: true })).toHaveValue("high");
    await expect(triggers.getByLabel("History retention in days")).toHaveValue("120");
    await expect(triggers.getByLabel("Waiting request expiry in hours")).toHaveValue("48");
    await expect(triggers.getByLabel("Polling interval in seconds")).toHaveValue("10");
    await expect(
      triggers.getByRole("radio", { name: "Edit a status footer into my comment" }),
    ).toBeChecked();
    for (const choice of ["Off", "Reactions", "Edit a status footer into my comment"]) {
      await triggers.getByRole("radio", { name: choice, exact: true }).check();
      await triggers.getByRole("button", { name: "Save", exact: true }).click();
      await expect(
        page.getByText("GitHub trigger settings saved", { exact: true }).last(),
      ).toBeVisible();
      await page.reload();
      await expect(triggers.getByRole("radio", { name: choice, exact: true })).toBeChecked();
    }

    await triggers.getByLabel("History retention in days").fill("");
    await triggers.getByRole("button", { name: "Save", exact: true }).click();
    await expect(triggers.getByRole("alert")).toHaveText(
      "Retention must be a whole number of days from 0 to 4294967295.",
    );
    await triggers.getByLabel("History retention in days").fill("120");

    await triggers.getByLabel("Waiting request expiry in hours").fill("");
    await triggers.getByRole("button", { name: "Save", exact: true }).click();
    await expect(triggers.getByRole("alert")).toHaveText(
      "Waiting expiry must be a whole number of hours from 1 to 4294967295.",
    );
    await triggers.getByLabel("Waiting request expiry in hours").fill("48");

    await triggers.getByRole("button", { name: "Add shortcut" }).click();
    await triggers.getByLabel("Command", { exact: true }).last().fill("/ezra-fast");
    await triggers.getByRole("button", { name: "Save", exact: true }).click();
    await expect(triggers.getByRole("alert")).toHaveText("Each shortcut command must be unique.");
  } finally {
    await request.put(path, { data: {} });
  }
});

const NO_CODEX_SUGGESTIONS =
  "Codex suggestions appear once Codex is running. You can still type model and effort values.";
const NO_CLAUDE_SUGGESTIONS =
  "Claude Code suggestions appear once its ~/projects server is running. You can still type model and effort values.";

/** A model as an agent lists it, with efforts that have no description. */
function listed(model: string, display_name: string, description: string, efforts: string[]) {
  return {
    model,
    display_name,
    description,
    efforts: efforts.map((effort) => ({ effort, description: "" })),
  };
}

/**
 * Serves `models` in the agents list, and returns a function that tells the page the agents
 * changed, as the manager does once an agent has listed its models.
 */
async function listModels(
  page: Page,
  models: Partial<Record<"claude" | "codex", object[]>>,
): Promise<() => Promise<void>> {
  const events: Route[] = [];
  await page.route("**/api/v1/events", (route) => {
    events.push(route);
  });
  await page.route("**/api/v1/agents", async (route) => {
    const response = await route.fetch();
    const agents = (await response.json()) as { agent: "claude" | "codex" }[];
    await route.fulfill({
      response,
      json: agents.map((agent) => ({ ...agent, models: models[agent.agent] ?? [] })),
    });
  });
  return async () => {
    await expect.poll(() => events.length).toBeGreaterThan(0);
    await events.shift()?.fulfill({
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ event: "changed", topic: "agents", revision: 1 })}\n\n`,
    });
  };
}

test("shortcut model and effort suggestions follow the selected model and accept custom values", async ({
  page,
  request,
}) => {
  const announce = await listModels(page, {
    codex: [
      listed("first-model", "First", "", ["low", "high"]),
      listed("second-model", "Second", "", ["future-effort"]),
    ],
  });
  try {
    await page.goto("./settings");
    const triggers = card(page, "GitHub triggers");
    const model = triggers.getByRole("combobox", { name: "Chat model", exact: true });
    const effort = triggers.getByRole("combobox", { name: "Chat effort", exact: true });
    const unlisted = triggers.getByText(NO_CODEX_SUGGESTIONS, { exact: true });
    await expect(unlisted).toBeVisible();
    await announce();
    await expect(unlisted).toBeHidden();
    await triggers.getByRole("button", { name: "Show Codex models", exact: true }).click();
    await page.getByRole("option", { name: "first-model First", exact: true }).click();
    await expect(model).toHaveValue("first-model");
    await triggers.getByRole("button", { name: "Show Codex effort levels", exact: true }).click();
    await expect(page.getByRole("option", { name: "future-effort", exact: true })).toHaveCount(0);
    await page.getByRole("option", { name: "high", exact: true }).click();
    await expect(effort).toHaveValue("high");
    await model.fill("second-model");
    await page.keyboard.press("Escape");
    await effort.fill("");
    await page.keyboard.press("Escape");
    await triggers.getByRole("button", { name: "Show Codex effort levels", exact: true }).click();
    await expect(page.getByRole("option", { name: "high", exact: true })).toHaveCount(0);
    await page.getByRole("option", { name: "future-effort", exact: true }).click();
    await model.fill("custom-model");
    await effort.fill("custom-effort");
    await triggers.getByRole("button", { name: "Save", exact: true }).click();
    await expect(
      page.getByText("GitHub trigger settings saved", { exact: true }).last(),
    ).toBeVisible();
    await page.reload();
    await expect(model).toHaveValue("custom-model");
    await expect(effort).toHaveValue("custom-effort");
  } finally {
    await request.put("api/v1/inbound/settings", { data: {} });
  }
});

test("manual shortcut values remain usable before an agent lists its models", async ({
  page,
  request,
}) => {
  try {
    await page.goto("./settings");
    const triggers = card(page, "GitHub triggers");
    await expect(triggers.getByText(NO_CODEX_SUGGESTIONS, { exact: true })).toBeVisible();
    await triggers.getByRole("combobox", { name: "Chat model", exact: true }).fill("manual-model");
    await triggers
      .getByRole("combobox", { name: "Chat effort", exact: true })
      .fill("manual-effort");
    await triggers.getByRole("button", { name: "Save", exact: true }).click();
    await expect(
      page.getByText("GitHub trigger settings saved", { exact: true }).last(),
    ).toBeVisible();
  } finally {
    await request.put("api/v1/inbound/settings", { data: {} });
  }
});

test("a shortcut's agent picks its suggestions", async ({ page }) => {
  const announce = await listModels(page, {
    claude: [
      listed("opus", "Opus", "For complex work", ["low", "max"]),
      listed("haiku", "Haiku", "Fastest", []),
    ],
  });
  await page.goto("./settings");
  const triggers = card(page, "GitHub triggers");
  const agent = triggers.getByRole("combobox", { name: "Agent", exact: true });
  const agentValue = agent.locator("[data-slot=select-value]");
  const model = triggers.getByRole("combobox", { name: "Chat model", exact: true });
  const effort = triggers.getByRole("combobox", { name: "Chat effort", exact: true });
  const noCodex = triggers.getByText(NO_CODEX_SUGGESTIONS, { exact: true });
  const noClaude = triggers.getByText(NO_CLAUDE_SUGGESTIONS, { exact: true });
  await expect(agentValue).toHaveText("Codex");
  await expect(noCodex).toBeVisible();
  await expect(noClaude).toBeHidden();
  await model.fill("codex-model");
  await effort.fill("high");

  await agent.click();
  await page.getByRole("option", { name: "Claude Code", exact: true }).click();
  await expect(agentValue).toHaveText("Claude Code");
  await expect(model).toHaveValue("");
  await expect(effort).toHaveValue("");
  await expect(noCodex).toBeHidden();
  await expect(noClaude).toBeVisible();
  await announce();
  await expect(noClaude).toBeHidden();

  await triggers.getByRole("button", { name: "Show Claude models", exact: true }).click();
  await expect(page.getByRole("option")).toHaveCount(2);
  await page.getByRole("option", { name: "haiku Fastest", exact: true }).click();
  await expect(model).toHaveValue("haiku");
  await triggers.getByRole("button", { name: "Show Claude effort levels", exact: true }).click();
  await expect(page.getByRole("option")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await model.fill("");
  await page.keyboard.press("Escape");
  await triggers.getByRole("button", { name: "Show Claude models", exact: true }).click();
  await page.getByRole("option", { name: "opus For complex work", exact: true }).click();
  await expect(model).toHaveValue("opus");
  await triggers.getByRole("button", { name: "Show Claude effort levels", exact: true }).click();
  await expect(page.getByRole("option")).toHaveText(["low", "max"]);
  await page.getByRole("option", { name: "max", exact: true }).click();
  await expect(effort).toHaveValue("max");

  await triggers.getByRole("button", { name: "Add shortcut" }).click();
  await expect(agentValue.last()).toHaveText("Codex");
  await expect(noCodex).toBeVisible();
});
