import { listClonesOptions, listFoldersOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

export const foldersQueryOptions = queryOptions({ ...listFoldersOptions(), staleTime: 30_000 });

/** Clones running or failed. The manager pushes each change, progress included. */
export const clonesQueryOptions = queryOptions({ ...listClonesOptions(), staleTime: 30_000 });
