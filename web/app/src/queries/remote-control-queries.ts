import { getRemoteControlOptions } from "@ezra/client/react-query.gen";
import { queryOptions } from "@tanstack/react-query";

export const remoteControlQueryOptions = queryOptions({
  ...getRemoteControlOptions(),
  staleTime: 30_000,
});
