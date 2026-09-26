import { Tooltip } from "@base-ui/react/tooltip";
import type { QueryClient } from "@tanstack/react-query";
import { Outlet, createRootRouteWithContext } from "@tanstack/react-router";

export interface RouterContext {
  queryClient: QueryClient;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootLayout,
});

function RootLayout() {
  return (
    <Tooltip.Provider>
      <main className="mx-auto max-w-5xl px-4 pt-6 pb-12">
        <Outlet />
      </main>
    </Tooltip.Provider>
  );
}
