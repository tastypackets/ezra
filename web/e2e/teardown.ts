import { execFileSync } from "node:child_process";

import { CONTAINER_NAME } from "./playwright.config.ts";

/** Removes the manager container even when Playwright stops its process abruptly. */
export default function removeManagerContainer(): void {
  execFileSync("docker", ["rm", "--force", CONTAINER_NAME], { stdio: "ignore" });
}
