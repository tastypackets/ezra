import { getManagerOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

export const managerQueryOptions = queryOptions({ ...getManagerOptions(), staleTime: 30_000 });
