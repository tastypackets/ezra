import { chooseToServeFolder } from "@ezra/client";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { foldersQueryOptions } from "@/queries/folder-queries";

/** Choosing whether the Claude app lists a folder. Refreshes the folders when it settles. */
export function useFolderActions(name: string) {
  const queryClient = useQueryClient();
  const chooseToServe = useMutation({
    mutationFn: async (serve: boolean) =>
      chooseToServeFolder({ path: { name }, body: { serve }, throwOnError: true }),
    onSettled: () => queryClient.invalidateQueries({ queryKey: foldersQueryOptions.queryKey }),
  });
  return { chooseToServe };
}
