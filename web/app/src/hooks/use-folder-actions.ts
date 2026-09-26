import { chooseToServeFolderMutation } from "@ezra/client/react-query.gen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { foldersQueryOptions } from "@/queries/folder-queries";

/** Choosing whether the Claude app lists a folder. Refreshes the folders when it settles. */
export function useFolderActions() {
  const queryClient = useQueryClient();
  const chooseToServe = useMutation({
    ...chooseToServeFolderMutation(),
    onSettled: () => queryClient.invalidateQueries({ queryKey: foldersQueryOptions.queryKey }),
  });
  return { chooseToServe };
}
