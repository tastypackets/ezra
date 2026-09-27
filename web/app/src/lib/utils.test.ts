import { describe, expect, it } from "vitest";

import { APP_DESCRIPTIONS } from "@/content/app";

import { capitalized, downloadPercent, errorMessage } from "./utils";

describe("downloadPercent", () => {
  it("needs a total", () => {
    expect(downloadPercent({ received_bytes: 5 })).toBeUndefined();
    expect(downloadPercent({ received_bytes: 5, total_bytes: null })).toBeUndefined();
    expect(downloadPercent({ received_bytes: 5, total_bytes: 0 })).toBeUndefined();
  });

  it("rounds down and stops at 100", () => {
    expect(downloadPercent({ received_bytes: 0, total_bytes: 200 })).toBe(0);
    expect(downloadPercent({ received_bytes: 51, total_bytes: 200 })).toBe(25);
    expect(downloadPercent({ received_bytes: 300, total_bytes: 200 })).toBe(100);
  });
});

describe("errorMessage", () => {
  it("prefers the manager's own message", () => {
    expect(errorMessage({ error: "wrong password" })).toBe("wrong password");
  });

  it("falls back to the code error or a generic failure", () => {
    expect(errorMessage(new Error("boom"))).toBe("boom");
    expect(errorMessage("<html>")).toBe(APP_DESCRIPTIONS.request_failed);
    expect(errorMessage({})).toBe(APP_DESCRIPTIONS.request_failed);
  });
});

describe("capitalized", () => {
  it("starts the text with a capital letter", () => {
    expect(capitalized("trailing comma")).toBe("Trailing comma");
    expect(capitalized("Unexpected token 'N'")).toBe("Unexpected token 'N'");
    expect(capitalized("")).toBe("");
  });
});
