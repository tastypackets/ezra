import type { AgentStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { nextAgentStep } from "./agent-steps";

const missing: AgentStatus = { agent: "claude", configured: false, logged_in: false };
const signedOut: AgentStatus = { ...missing, configured: true, installed_version: "2.1.283" };
const signedIn: AgentStatus = { ...signedOut, logged_in: true };
const prompt = { url: "https://claude.com/oauth" };

describe("nextAgentStep", () => {
  it("installs a missing agent", () => {
    expect(nextAgentStep(missing, false)).toBe("install");
  });

  it("keeps the install step while a download runs", () => {
    expect(nextAgentStep(signedIn, true)).toBe("install");
    expect(nextAgentStep(signedOut, true)).toBe("install");
  });

  it("signs in before offering an update", () => {
    expect(nextAgentStep(signedOut, false)).toBe("sign_in");
    expect(nextAgentStep({ ...signedOut, available_update: "2.1.284" }, false)).toBe("sign_in");
  });

  it("offers a known update once signed in or signing in", () => {
    expect(nextAgentStep({ ...signedIn, available_update: "2.1.284" }, false)).toBe("install");
    expect(
      nextAgentStep({ ...signedOut, login_prompt: prompt, available_update: "2.1.284" }, false),
    ).toBe("install");
  });

  it("has no step for an agent that is signed in and up to date, or signing in", () => {
    expect(nextAgentStep(signedIn, false)).toBeUndefined();
    expect(nextAgentStep({ ...signedOut, login_prompt: prompt }, false)).toBeUndefined();
  });
});
