import { getSession } from "@ezra/client";
import { queryOptions } from "@tanstack/react-query";

import { SESSION } from "./query-keys";

/** Changes only through this browser's own setup, sign-in and sign-out, so it is cached for the visit. */
export const sessionQueryOptions = queryOptions({
  queryKey: [SESSION],
  queryFn: async () => (await getSession({ throwOnError: true })).data,
  staleTime: 5 * 60_000,
});
