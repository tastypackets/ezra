import { client } from "@ezra/client/client.gen";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { APP_DESCRIPTIONS } from "@/content/app";
import { seedInitialData } from "@/lib/initial-data";
import { queryClient } from "@/lib/query-client";

import { router } from "./router";
import "./styles.css";

const initialRevision = seedInitialData(queryClient);

// A 401 anywhere but the sign-in form means the manager session ended, e.g. after a restart.
client.interceptors.response.use((response) => {
  if (response.status === 401 && window.location.pathname !== "/login") {
    window.location.assign("/login");
    return new Promise<Response>(() => {});
  }
  return response;
});

// No response means fetch itself failed.
client.interceptors.error.use((error, response) =>
  response ? error : { error: APP_DESCRIPTIONS.unreachable },
);

const rootElement = document.getElementById("app");
if (rootElement) {
  createRoot(rootElement).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} context={{ initialRevision }} />
      </QueryClientProvider>
    </StrictMode>,
  );
}
