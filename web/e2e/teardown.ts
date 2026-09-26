import { execFileSync } from "node:child_process";

/** Removes the manager container even when Playwright stops its process abruptly. */
export default function removeManagerContainer(): void {
  const container = process.env["EZRA_E2E_CONTAINER"];
  if (container) {
    execFileSync("docker", ["rm", "--force", container], { stdio: "ignore" });
  }
}
