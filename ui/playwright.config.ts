import { defineConfig, devices } from "@playwright/test";

const PORT = 18443;
export const CONTAINER_NAME = "agent-box-e2e";

export default defineConfig({
  testDir: "e2e",
  fullyParallel: false,
  workers: 1,
  globalTeardown: "./e2e/teardown.ts",
  use: {
    baseURL: `https://127.0.0.1:${PORT}/`,
    ignoreHTTPSErrors: true,
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `docker rm --force ${CONTAINER_NAME} >/dev/null 2>&1; docker run --rm --name ${CONTAINER_NAME} -p 127.0.0.1:${PORT}:8443 agent-box:dev`,
    url: `https://127.0.0.1:${PORT}/api/v1/session`,
    ignoreHTTPSErrors: true,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
