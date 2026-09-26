import type { SpawnMode } from "@ezra/client";

export const FOLDERS_DESCRIPTIONS = {
  title: "Folders",
  description: "An agent started in /projects can also clone and set up repositories.",
  empty: "No projects yet. Clone a repository, or ask an agent started in /projects to.",
  projects: "All projects",
  projects_path: "/projects",
  not_git: "Not a git repository",
  serve_label: (folder: string) => `Serve ${folder} in the Claude app`,
  worktrees: (count: number) => (count === 1 ? "1 worktree" : `${count} worktrees`),
  more_actions: (folder: string) => `More ${folder} actions`,
  open: (folder: string) => `Open ${folder} on claude.ai/code`,
  claude_options: "Claude Code options",
  worktree_needs_repository: "Needs a git repository.",
  delete: "Delete",
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
  folder_cloning: "A clone into this folder is running.",
  serve: "Serve in the Claude app",
  cancel: "Cancel",
  submit: "Clone",
} as const;

export const SPAWN_MODES: Record<SpawnMode, { title: string; description: string }> = {
  "same-dir": {
    title: "The folder",
    description: "Sessions share the folder and its branch.",
  },
  worktree: {
    title: "Their own worktree",
    description: "Each on a new branch in .claude/worktrees.",
  },
};

export const CLAUDE_OPTIONS_DESCRIPTIONS = {
  title: (folder: string) => `Claude Code in ${folder}`,
  description: "Empty fields follow Settings.",
  spawn: "New sessions work in",
  permission_mode: "Permission mode",
  permission_mode_default: (mode: string) => `Default: ${mode}`,
  permission_mode_unknown: "Choose a listed mode, or leave it empty.",
  capacity: "Sessions at once",
  capacity_default: (capacity: number | null | undefined) =>
    capacity == null ? "Default: Claude Code's" : `Default: ${capacity}`,
  capacity_range: "Enter a whole number of 1 or more, or leave it empty.",
  cancel: "Cancel",
  save: "Save",
  saved: (folder: string) => `Saved the Claude Code options for ${folder}.`,
} as const;

export const DELETE_FOLDER_DESCRIPTIONS = {
  title: (folder: string) => `Delete ${folder}?`,
  description: (folder: string) => `Deletes /projects/${folder} and everything in it.`,
  server: "Stops its Remote Control server and the sessions in it.",
  checking: "Checking for unsaved work",
  check_failed: "Could not check for unsaved work",
  only_here: "Only in this folder:",
  uncommitted_changes: (count: number) =>
    count === 1 ? "1 uncommitted change" : `${count} uncommitted changes`,
  unpushed_commits: (count: number) =>
    count === 1 ? "1 commit on no remote" : `${count} commits on no remote`,
  stashes: (count: number) => (count === 1 ? "1 stash" : `${count} stashes`),
  cancel: "Cancel",
  confirm: "Delete",
  deleted: (folder: string) => `Deleted ${folder}.`,
} as const;
