import type { AgentStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { menuOffersInstall, nextAgentStep } from "./agent-steps";

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

  it("signs in again, before an update, when the sign-in is about to end", () => {
    expect(nextAgentStep(signedIn, false, true)).toBe("sign_in");
    expect(nextAgentStep({ ...signedIn, available_update: "2.1.284" }, false, true)).toBe(
      "sign_in",
    );
    expect(nextAgentStep({ ...signedIn, login_prompt: prompt }, false, true)).toBeUndefined();
  });

  it("has no step for an agent that is signed in and up to date, or signing in", () => {
    expect(nextAgentStep(signedIn, false)).toBeUndefined();
    expect(nextAgentStep({ ...signedOut, login_prompt: prompt }, false)).toBeUndefined();
  });

  it("installs and signs in before the Codex fix", () => {
    expect(nextAgentStep(missing, false, false, "try_again")).toBe("install");
    expect(nextAgentStep(signedIn, true, false, "try_again")).toBe("install");
    expect(nextAgentStep(signedOut, false, false, "try_again")).toBe("sign_in");
    expect(nextAgentStep(signedIn, false, true, "sign_in_with_chatgpt")).toBe("sign_in");
  });

  it("offers each Codex fix once installed and signed in", () => {
    for (const fix of ["try_again", "sign_in_with_chatgpt", "sign_in"] as const) {
      expect(nextAgentStep(signedIn, false, false, fix)).toBe(fix);
    }
  });

  it("offers the Codex fix before an update", () => {
    expect(
      nextAgentStep({ ...signedIn, available_update: "2.1.284" }, false, false, "try_again"),
    ).toBe("try_again");
  });

  it("has no Codex fix while signing in", () => {
    expect(
      nextAgentStep({ ...signedIn, login_prompt: prompt }, false, false, "sign_in"),
    ).toBeUndefined();
  });
});

describe("menuOffersInstall", () => {
  it("keeps the install step in a signed-out agent's menu while it runs", () => {
    expect(menuOffersInstall(signedOut, nextAgentStep(signedOut, false))).toBe(true);
    expect(menuOffersInstall(signedOut, nextAgentStep(signedOut, true))).toBe(true);
  });

  it("leaves the install step to the button when Sign out fills the menu", () => {
    const update = { ...signedIn, available_update: "2.1.284" };
    expect(menuOffersInstall(update, nextAgentStep(update, false))).toBe(false);
    expect(menuOffersInstall(signedIn, nextAgentStep(signedIn, true))).toBe(false);
    expect(menuOffersInstall(signedIn, nextAgentStep(signedIn, false))).toBe(true);
  });

  it("offers an update in the menu while the button shows a Codex fix", () => {
    const update = { ...signedIn, available_update: "2.1.284" };
    expect(menuOffersInstall(update, nextAgentStep(update, false, false, "try_again"))).toBe(true);
  });

  it("offers the install step to an agent signing in, even when the button shows it", () => {
    const signingIn = { ...signedOut, login_prompt: prompt, available_update: "2.1.284" };
    expect(menuOffersInstall(signingIn, nextAgentStep(signingIn, false))).toBe(true);
  });
});
