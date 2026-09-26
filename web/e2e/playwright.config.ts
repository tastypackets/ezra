import { randomInt, randomUUID } from "node:crypto";

import { defineConfig, devices } from "@playwright/test";

process.env["EZRA_E2E_CONTAINER"] ??= `ezra-e2e-${randomUUID().slice(0, 8)}`;
process.env["EZRA_E2E_PORT"] ??= String(randomInt(20_000, 30_000));

const CONTAINER = process.env["EZRA_E2E_CONTAINER"];
const PORT = process.env["EZRA_E2E_PORT"];
const IMAGE = process.env["EZRA_TEST_IMAGE"] ?? "ezra:dev";
export const SESSION_FILE = "tests/.auth/session.json";

export default defineConfig({
  testDir: "tests",
  fullyParallel: false,
  workers: 1,
  retries: 1,
  globalTeardown: "./teardown.ts",
  use: {
    baseURL: `https://127.0.0.1:${PORT}/`,
    ignoreHTTPSErrors: true,
  },
  projects: [
    { name: "setup", testMatch: /\.setup\.ts$/ },
    {
      name: "chromium",
      dependencies: ["setup"],
      use: { ...devices["Desktop Chrome"], storageState: SESSION_FILE },
    },
  ],
  webServer: {
    command: `docker run --rm --pull never --name ${CONTAINER} -p 127.0.0.1:${PORT}:8443 ${IMAGE}`,
    url: `https://127.0.0.1:${PORT}/api/v1/session`,
    ignoreHTTPSErrors: true,
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
