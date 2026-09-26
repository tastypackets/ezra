import { client } from "@ezra/client/client.gen";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { seedInitialData } from "@/lib/initial-data";
import { queryClient } from "@/lib/query-client";
import { sessionQueryOptions } from "@/queries/session-queries";

import { router } from "./router";
import "./styles.css";

seedInitialData(queryClient);

// A 401 anywhere but the sign-in form means the manager session ended, e.g. after a restart.
client.interceptors.response.use((response) => {
  if (response.status === 401 && router.state.location.pathname !== "/login") {
    queryClient.setQueryData(sessionQueryOptions.queryKey, { claimed: true, authenticated: false });
    void router.navigate({ to: "/login" });
  }
  return response;
});

const rootElement = document.getElementById("app");
if (rootElement) {
  createRoot(rootElement).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </StrictMode>,
  );
}
