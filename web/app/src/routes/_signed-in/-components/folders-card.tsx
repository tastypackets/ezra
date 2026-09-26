import type { FolderStatus } from "@ezra/client";
import { useQuery } from "@tanstack/react-query";

import { Card, CardHeader } from "@/components/ui/card";
import { Spinner } from "@/components/ui/spinner";
import { FOLDERS_DESCRIPTIONS } from "@/content/folders";
import { errorMessage } from "@/lib/utils";
import { foldersQueryOptions } from "@/queries/folder-queries";

/** The projects agents work in. */
export function FoldersCard() {
  const folders = useQuery(foldersQueryOptions);
  return (
    <Card>
      <CardHeader
        title={FOLDERS_DESCRIPTIONS.title}
        description={FOLDERS_DESCRIPTIONS.description}
      />
      {folders.isPending ? (
        <div role="status" className="flex justify-center px-5 py-4 text-ez-muted">
          <Spinner />
        </div>
      ) : folders.isError ? (
        <p role="alert" className="px-5 py-4 text-ez-danger">
          {errorMessage(folders.error)}
        </p>
      ) : folders.data.length === 0 ? (
        <p className="px-5 py-4 text-ez-muted">{FOLDERS_DESCRIPTIONS.empty}</p>
      ) : (
        <ul className="divide-y divide-ez-border">
          {folders.data.map((folder) => (
            <FolderRow key={folder.name} folder={folder} />
          ))}
        </ul>
      )}
    </Card>
  );
}

function FolderRow({ folder }: { folder: FolderStatus }) {
  const detail = folder.git ? folder.git.repository : FOLDERS_DESCRIPTIONS.not_git;
  return (
    <li className="flex flex-col gap-0.5 px-5 py-3">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3">
        <span className="min-w-0 font-medium break-all">{folder.name}</span>
        {folder.git?.branch ? (
          <span className="min-w-0 font-mono text-[0.8125rem] break-all text-ez-muted">
            {folder.git.branch}
          </span>
        ) : null}
      </div>
      {detail ? <span className="truncate text-ez-muted">{detail}</span> : null}
    </li>
  );
}
