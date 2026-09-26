import { describe, expect, it } from "vitest";

import { isInitialData } from "./initial-data";

describe("isInitialData", () => {
  it("accepts what the manager writes", () => {
    const session = { claimed: true, authenticated: false };
    expect(isInitialData({ session, agents: null })).toBe(true);
    expect(isInitialData({ session, agents: [] })).toBe(true);
  });

  it("rejects anything else", () => {
    expect(isInitialData(null)).toBe(false);
    expect(isInitialData({ session: null, agents: null })).toBe(false);
    expect(isInitialData({ session: { claimed: true }, agents: null })).toBe(false);
    expect(isInitialData({ session: { claimed: true, authenticated: true } })).toBe(false);
    expect(isInitialData({ session: { claimed: true, authenticated: true }, agents: {} })).toBe(
      false,
    );
  });
});
