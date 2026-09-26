import { execFileSync } from "node:child_process";

import type { Page } from "@playwright/test";

export const PASSWORD = "correct horse";

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
