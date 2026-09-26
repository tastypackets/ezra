import { listFoldersOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

export const foldersQueryOptions = queryOptions({ ...listFoldersOptions(), staleTime: 30_000 });
