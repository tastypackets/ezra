import { describe, expect, it } from "vitest";

import { countdown, phoneName } from "./pairing";

const NOW = new Date("2026-09-27T12:00:00Z");

describe("countdown", () => {
  it("shows minutes and seconds left", () => {
    expect(countdown("2026-09-27T12:09:41Z", NOW)).toBe("9:41");
    expect(countdown("2026-09-27T12:00:05Z", NOW)).toBe("0:05");
  });

  it("counts a started second as left", () => {
    expect(countdown("2026-09-27T12:00:04.200Z", NOW)).toBe("0:05");
  });

  it("stops at zero once the code expired", () => {
    expect(countdown("2026-09-27T12:00:00Z", NOW)).toBe("0:00");
    expect(countdown("2026-09-27T11:58:00Z", NOW)).toBe("0:00");
  });
});

describe("phoneName", () => {
  it("prefers the name, then the model, then the platform", () => {
    expect(phoneName({ id: "1", name: "Zeke's iPhone", model: "iPhone17,1" })).toBe(
      "Zeke's iPhone",
    );
    expect(phoneName({ id: "1", name: " ", model: "iPhone17,1", platform: "iOS" })).toBe(
      "iPhone17,1",
    );
    expect(phoneName({ id: "1", platform: "iOS" })).toBe("iOS");
  });

  it("falls back to Phone", () => {
    expect(phoneName({ id: "1" })).toBe("Phone");
  });
});
