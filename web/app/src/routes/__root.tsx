import type { QueryClient } from "@tanstack/react-query";
import { Outlet, createRootRouteWithContext } from "@tanstack/react-router";

import { AppHeader } from "@/components/app-header";
import { Toaster } from "@/components/ui/toast";
import { APP_DESCRIPTIONS } from "@/content/app";

export interface RouterContext {
  queryClient: QueryClient;
  /** The event revision the data in `index.html` is from, when the page had some. */
  initialRevision?: number;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootLayout,
});

function RootLayout() {
  return (
    <Toaster closeLabel={APP_DESCRIPTIONS.dismiss}>
      <main className="mx-auto max-w-5xl px-4 pt-6 pb-12">
        <AppHeader />
        <Outlet />
      </main>
    </Toaster>
  );
}
