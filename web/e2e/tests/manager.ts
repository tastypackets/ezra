import { execFileSync } from "node:child_process";

import { expect } from "@playwright/test";
import type { APIRequestContext, Page } from "@playwright/test";

export const PASSWORD = "correct horse";

const CLAUDE_VERSIONS = "/home/dev/.local/share/claude/versions";
const CLAUDE_COMMAND = "/home/dev/.local/bin/claude";
export const CLAUDE_CREDENTIALS = "/config/claude/.credentials.json";
const CODEX_VERSIONS = "/home/dev/.local/share/codex";
const CODEX_COMMAND = "/home/dev/.local/bin/codex";

function container(): string {
  const name = process.env["EZRA_E2E_CONTAINER"];
  if (!name) {
    throw new Error("EZRA_E2E_CONTAINER is not set");
  }
  return name;
}

/** Runs a command as the container's user, such as making a folder in /projects, and returns its output. */
export function inContainer(...command: string[]): string {
  return execFileSync("docker", ["exec", "-u", "dev", container(), ...command], {
    encoding: "utf8",
  });
}

/** Writes a file as the container's user, making its folder first. */
export function writeInContainer(path: string, contents: string, mode = "644"): void {
  execFileSync(
    "docker",
    [
      "exec",
      "-i",
      "-u",
      "dev",
      container(),
      "sh",
      "-c",
      'mkdir -p "$(dirname "$1")" && cat > "$1" && chmod "$2" "$1"',
      "sh",
      path,
      mode,
    ],
    { input: contents },
  );
}

/** Collects the manager API calls a page makes, to check the first screen needed none. */
export function recordApiCalls(page: Page): string[] {
  const calls: string[] = [];
  page.on("request", (request) => {
    const { pathname } = new URL(request.url());
    if (pathname.startsWith("/api/")) {
      calls.push(`${request.method()} ${pathname}`);
    }
  });
  return calls;
}

/** Links a `claude` script as `version`, signed in with Remote Control running `server` when given, signed out otherwise. */
export function installFakeClaude(version: string, server?: string): void {
  const signIn = server
    ? `"auth status") echo '{"loggedIn":true,"email":"e2e@example.com","subscriptionType":"max"}' ;;
  remote-control*) ${server} ;;`
    : `"auth status") echo '{"loggedIn":false}' ;;`;
  writeInContainer(
    `${CLAUDE_VERSIONS}/${version}`,
    `#!/bin/sh
case "$1 $2" in
  ${signIn}
  "auth login") echo 'Visit https://claude.ai/oauth/authorize?code=true to sign in'; exec sleep 300 ;;
esac
`,
    "755",
  );
  inContainer("mkdir", "-p", "/home/dev/.local/bin");
  inContainer("ln", "-sfn", `${CLAUDE_VERSIONS}/${version}`, CLAUDE_COMMAND);
}

/** A settings change makes the manager check Claude Code's sign-in and servers at once. */
export async function nudgeRemoteControl(request: APIRequestContext): Promise<void> {
  const settings = await (await request.get("api/v1/agents/claude/settings")).json();
  for (const serve_repositories of [!settings.remote_control.serve_repositories, true]) {
    const saved = await request.put("api/v1/agents/claude/settings", {
      data: { ...settings, remote_control: { ...settings.remote_control, serve_repositories } },
    });
    expect(saved.ok()).toBe(true);
  }
}

export async function removeFakeClaude(request: APIRequestContext): Promise<void> {
  await request.post("api/v1/agents/claude/logout");
  inContainer("rm", "-rf", CLAUDE_COMMAND, CLAUDE_VERSIONS, CLAUDE_CREDENTIALS);
  await nudgeRemoteControl(request);
}

/** Links a `codex` script as `version` with the case branches in `signIn`. It takes every flag, its sandbox finds the home folder, and its server runs without answering. */
export function installFakeCodex(version: string, signIn: string): void {
  writeInContainer(
    `${CODEX_VERSIONS}/${version}/bin/codex`,
    `#!/bin/sh
case "$*" in
  *--help) exit 0 ;;
${signIn}
  sandbox*) echo /home/dev ;;
  app-server*) exec sleep 600 ;;
esac
`,
    "755",
  );
  inContainer("mkdir", "-p", "/home/dev/.local/bin");
  inContainer("ln", "-sfn", `${CODEX_VERSIONS}/${version}/bin/codex`, CODEX_COMMAND);
}

/** Saving Codex's settings unchanged makes the manager check its sign-in and server at once. */
export async function nudgeCodex(request: APIRequestContext): Promise<void> {
  const settings = await (await request.get("api/v1/agents/codex/settings")).json();
  const saved = await request.put("api/v1/agents/codex/settings", { data: settings });
  expect(saved.ok()).toBe(true);
}

export async function removeFakeCodex(request: APIRequestContext): Promise<void> {
  inContainer("rm", "-rf", CODEX_COMMAND, CODEX_VERSIONS);
  await nudgeCodex(request);
}
