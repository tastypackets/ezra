import { CLONE_DESCRIPTIONS } from "@/content/folders";

/** The folder name for a repository: the last part of its path, without `.git` or leading dots. */
export function defaultFolderName(repository: string): string {
  const path = repository
    .trim()
    .replace(/\/+$/, "")
    .replace(/\.git$/, "")
    .replace(/\/+$/, "");
  return (path.split(/[/:]/).at(-1) ?? "").replace(/^\.+/, "");
}

/** The folders in /projects and the running clones, which a new clone cannot use. */
export interface TakenNames {
  folders: readonly string[];
  cloning: readonly string[];
}

/** Why `name` cannot be a new folder in /projects, when it cannot. */
export function folderNameProblem(name: string, taken: TakenNames): string | undefined {
  const trimmed = name.trim();
  if (!trimmed) {
    return CLONE_DESCRIPTIONS.folder_required;
  }
  if (trimmed.startsWith(".") || trimmed.includes("/")) {
    return CLONE_DESCRIPTIONS.folder_invalid;
  }
  if (taken.folders.includes(trimmed)) {
    return CLONE_DESCRIPTIONS.folder_exists;
  }
  return taken.cloning.includes(trimmed) ? CLONE_DESCRIPTIONS.folder_cloning : undefined;
}
