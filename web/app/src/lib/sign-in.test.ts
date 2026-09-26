import type { AgentStatus } from "@ezra/client";
import { describe, expect, it } from "vitest";

import { signInEnd } from "./sign-in";

const now = new Date("2026-09-26T12:00:00Z");
const signedIn: AgentStatus = { agent: "claude", configured: true, logged_in: true };
const endingAt = (sign_in_ends_at: string) => signInEnd({ ...signedIn, sign_in_ends_at }, now);

describe("signInEnd", () => {
  it("says nothing while the end is unknown or more than three days away", () => {
    expect(signInEnd(signedIn, now)).toBeUndefined();
    expect(endingAt("2026-09-29T12:00:01Z")).toBeUndefined();
    expect(endingAt("not a date")).toBeUndefined();
  });

  it("warns within three days and says when it has ended", () => {
    expect(endingAt("2026-09-29T12:00:00Z")).toEqual({
      at: new Date("2026-09-29T12:00:00Z"),
      ended: false,
    });
    expect(endingAt("2026-09-26T12:00:00Z")?.ended).toBe(true);
    expect(endingAt("2026-09-20T00:00:00Z")?.ended).toBe(true);
  });

  it("ignores a signed-out agent", () => {
    expect(
      signInEnd({ ...signedIn, logged_in: false, sign_in_ends_at: "2026-09-27T00:00:00Z" }, now),
    ).toBeUndefined();
  });
});
