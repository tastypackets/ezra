import type { AgentStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { INSTALL_POLL_MS, isInstalled, pollInterval } from "./agent-queries";

const idle: AgentStatus = { agent: "claude", configured: false, logged_in: false };

describe("pollInterval", () => {
  it("does not poll while nothing is in progress", () => {
    expect(pollInterval(undefined)).toBe(false);
    expect(pollInterval([idle, { ...idle, agent: "codex" }])).toBe(false);
  });

  it("polls fast while a download runs", () => {
    const installing = { ...idle, install_progress: { received_bytes: 1, total_bytes: 10 } };
    expect(pollInterval([installing])).toBe(INSTALL_POLL_MS);
  });

  it("does not poll while a sign-in waits", () => {
    const codex = { url: "https://auth.openai.com/codex/device", code: "ABCD-1234" };
    const claude = { url: "https://claude.com/oauth" };
    expect(
      pollInterval([
        { ...idle, login_prompt: claude },
        { ...idle, agent: "codex", login_prompt: codex },
      ]),
    ).toBe(false);
  });
});

describe("isInstalled", () => {
  const codex: AgentStatus = { ...idle, agent: "codex" };
  const claudeInstalled = { ...idle, installed_version: "2.1.283" };
  const codexInstalled = { ...codex, installed_version: "0.157.1" };

  it("follows each agent's own installed version", () => {
    expect(isInstalled("claude")([idle, codexInstalled])).toBe(false);
    expect(isInstalled("claude")([claudeInstalled, codex])).toBe(true);
    expect(isInstalled("codex")([claudeInstalled, codex])).toBe(false);
    expect(isInstalled("codex")([idle, codexInstalled])).toBe(true);
  });

  it("is false for an agent missing from the list", () => {
    expect(isInstalled("codex")([claudeInstalled])).toBe(false);
  });
});
