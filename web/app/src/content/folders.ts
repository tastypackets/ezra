import type { SpawnMode } from "@ezra/client";

export const FOLDERS_DESCRIPTIONS = {
  title: "Folders",
  empty: "No projects yet.",
  not_git: "Not a git repository",
  serve: "Claude app",
  serve_label: (folder: string) => `Serve ${folder} in the Claude app`,
  worktrees: (count: number) => (count === 1 ? "1 worktree" : `${count} worktrees`),
  more_actions: (folder: string) => `More ${folder} actions`,
  spawn: "New sessions work in",
  worktree_needs_repository: "Needs a git repository.",
  cloning: "Cloning",
  clone_failed: "Could not clone",
  stop_clone: "Stop",
  stop_clone_label: (folder: string) => `Stop cloning ${folder}`,
  dismiss: "Dismiss",
  dismiss_label: (folder: string) => `Dismiss the failed clone of ${folder}`,
} as const;

export const CLONE_DESCRIPTIONS = {
  open: "Clone repository",
  title: "Clone repository",
  repository: "Repository",
  repository_hint: "A git URL, or owner/repo for GitHub.",
  repository_required: "Enter a repository.",
  show_repositories: "Show your GitHub repositories",
  folder: "Folder",
  folder_hint: "The new folder in /projects.",
  folder_required: "Enter a folder name.",
  folder_invalid: "Use a name without slashes that does not start with a dot.",
  folder_exists: "A folder with this name exists.",
  serve: "Serve in the Claude app",
  cancel: "Cancel",
  submit: "Clone",
} as const;

export const SPAWN_MODES: Record<
  SpawnMode,
  { title: string; description: string; saved: (folder: string) => string }
> = {
  "same-dir": {
    title: "The folder",
    description: "Sessions share the folder and its branch.",
    saved: (folder) => `New ${folder} sessions work in the folder.`,
  },
  worktree: {
    title: "Their own worktree",
    description: "Each on a new branch in .claude/worktrees.",
    saved: (folder) => `New ${folder} sessions get their own worktree.`,
  },
};
