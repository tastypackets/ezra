import { getSessionOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

/** Changes only through this browser's own setup, sign-in and sign-out, so it is cached for the visit. */
export const sessionQueryOptions = queryOptions({ ...getSessionOptions(), staleTime: 5 * 60_000 });
