export const FOLDERS_DESCRIPTIONS = {
  title: "Folders",
  empty: "No projects yet. Ask an agent to clone a repository into /projects.",
  not_git: "Not a git repository",
  serve: "Claude app",
  serve_label: (folder: string) => `Serve ${folder} in the Claude app`,
} as const;
