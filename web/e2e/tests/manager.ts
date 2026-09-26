import { execFileSync } from "node:child_process";

import type { Page } from "@playwright/test";

export const PASSWORD = "correct horse";

/** Runs a command as the container's user, such as making a folder in /projects. */
export function inContainer(...command: string[]): void {
  const container = process.env["EZRA_E2E_CONTAINER"];
  if (!container) {
    throw new Error("EZRA_E2E_CONTAINER is not set");
  }
  execFileSync("docker", ["exec", "-u", "dev", container, ...command]);
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
