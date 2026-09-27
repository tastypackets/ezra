import type { CodexApprovals, CodexSandbox } from "@ezra/client";
import { describe, expect, expectTypeOf, it } from "vitest";

import {
  APPROVAL_POLICIES,
  APPROVAL_POLICY_ORDER,
  SANDBOX_MODE_ORDER,
  SANDBOX_MODES,
} from "./settings";

describe("Codex choices", () => {
  it("offer every sandbox once", () => {
    expectTypeOf<(typeof SANDBOX_MODE_ORDER)[number]>().toEqualTypeOf<CodexSandbox>();
    expect(SANDBOX_MODE_ORDER.toSorted()).toEqual(Object.keys(SANDBOX_MODES).toSorted());
  });

  it("offer every approval policy once", () => {
    expectTypeOf<(typeof APPROVAL_POLICY_ORDER)[number]>().toEqualTypeOf<CodexApprovals>();
    expect(APPROVAL_POLICY_ORDER.toSorted()).toEqual(Object.keys(APPROVAL_POLICIES).toSorted());
  });
});
