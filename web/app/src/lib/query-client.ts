import { QueryClient } from "@tanstack/react-query";

/** The app's single query cache, shared by the router context and components. */
export const queryClient = new QueryClient();
