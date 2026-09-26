import { createRouter } from "@tanstack/react-router";

import { ErrorState } from "@/components/error-state";
import { LoadingState } from "@/components/loading-state";
import { NotFoundState } from "@/components/not-found-state";
import { queryClient } from "@/lib/query-client";

import { routeTree } from "./routeTree.gen";

export const router = createRouter({
  routeTree,
  context: { queryClient },
  scrollRestoration: true,
  defaultPreloadStaleTime: 0,
  defaultPendingMs: 500,
  defaultPendingComponent: LoadingState,
  defaultErrorComponent: ErrorState,
  defaultNotFoundComponent: NotFoundState,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
