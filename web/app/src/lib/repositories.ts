import { CLONE_DESCRIPTIONS } from "@/content/folders";

/** The folder name git would pick for a repository: the last part of its path, without `.git`. */
export function defaultFolderName(repository: string): string {
  const path = repository
    .trim()
    .replace(/\/+$/, "")
    .replace(/\.git$/, "")
    .replace(/\/+$/, "");
  return (path.split(/[/:]/).at(-1) ?? "").replace(/^\.+/, "");
}

/** Why `name` cannot be a new folder in /projects, when it cannot. */
export function folderNameProblem(name: string, existing: readonly string[]): string | undefined {
  const trimmed = name.trim();
  if (!trimmed) {
    return CLONE_DESCRIPTIONS.folder_required;
  }
  if (trimmed.startsWith(".") || trimmed.includes("/")) {
    return CLONE_DESCRIPTIONS.folder_invalid;
  }
  return existing.includes(trimmed) ? CLONE_DESCRIPTIONS.folder_exists : undefined;
}
