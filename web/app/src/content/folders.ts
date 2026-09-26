import type { SpawnMode } from "@ezra/client";

export const FOLDERS_DESCRIPTIONS = {
  title: "Folders",
  empty: "No projects yet. Ask an agent to clone a repository into /projects.",
  not_git: "Not a git repository",
  serve: "Claude app",
  serve_label: (folder: string) => `Serve ${folder} in the Claude app`,
  worktrees: (count: number) => (count === 1 ? "1 worktree" : `${count} worktrees`),
  more_actions: (folder: string) => `More ${folder} actions`,
  spawn: "New sessions work in",
  worktree_needs_repository: "Needs a git repository.",
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
