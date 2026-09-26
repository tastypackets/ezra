import { listFolders } from "@ezra/client";
import { queryOptions } from "@tanstack/react-query";

import { FOLDERS } from "./query-keys";

export const foldersQueryOptions = queryOptions({
  queryKey: [FOLDERS],
  queryFn: async () => (await listFolders({ throwOnError: true })).data,
  staleTime: 5_000,
});
