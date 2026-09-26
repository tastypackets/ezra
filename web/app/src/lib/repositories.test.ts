import { describe, expect, it } from "vitest";

import { CLONE_DESCRIPTIONS } from "@/content/folders";

import { defaultFolderName, folderNameProblem } from "./repositories";

describe("defaultFolderName", () => {
  it("takes the last part of any repository", () => {
    expect(defaultFolderName("zeke/app")).toBe("app");
    expect(defaultFolderName("https://github.com/zeke/app.git")).toBe("app");
    expect(defaultFolderName("git@github.com:zeke/app.git")).toBe("app");
    expect(defaultFolderName("file:///srv/git/app/")).toBe("app");
    expect(defaultFolderName(" /srv/git/app/.git ")).toBe("app");
    expect(defaultFolderName("zeke/.dotfiles")).toBe("dotfiles");
  });

  it("is empty until there is a name", () => {
    expect(defaultFolderName("")).toBe("");
    expect(defaultFolderName("zeke/")).toBe("zeke");
  });
});

describe("folderNameProblem", () => {
  const none = { folders: [], cloning: [] };

  it("accepts a new top-level name", () => {
    expect(folderNameProblem("app", { folders: ["notes"], cloning: ["site"] })).toBeUndefined();
    expect(folderNameProblem(" my app ", none)).toBeUndefined();
  });

  it("refuses empty, hidden and nested names", () => {
    expect(folderNameProblem(" ", none)).toBe(CLONE_DESCRIPTIONS.folder_required);
    expect(folderNameProblem(".hidden", none)).toBe(CLONE_DESCRIPTIONS.folder_invalid);
    expect(folderNameProblem("a/b", none)).toBe(CLONE_DESCRIPTIONS.folder_invalid);
  });

  it("says whether a folder or a running clone holds the name", () => {
    expect(folderNameProblem("notes ", { folders: ["notes"], cloning: [] })).toBe(
      CLONE_DESCRIPTIONS.folder_exists,
    );
    expect(folderNameProblem("app", { folders: [], cloning: ["app"] })).toBe(
      CLONE_DESCRIPTIONS.folder_cloning,
    );
  });
});
