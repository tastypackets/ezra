import type { Page } from "@playwright/test";

export const PASSWORD = "correct horse";

/** Collects the manager API calls a page makes, to check the first screen needed none. */
export function recordApiCalls(page: Page): string[] {
  const calls: string[] = [];
  page.on("request", (request) => {
    const { pathname } = new URL(request.url());
    if (pathname.startsWith("/api/")) {
      calls.push(`${request.method()} ${pathname}`);
    }
  });
  return calls;
}
