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
  it("accepts a new top-level name", () => {
    expect(folderNameProblem("app", ["notes"])).toBeUndefined();
    expect(folderNameProblem(" my app ", [])).toBeUndefined();
  });

  it("refuses empty, hidden, nested and taken names", () => {
    expect(folderNameProblem(" ", [])).toBe(CLONE_DESCRIPTIONS.folder_required);
    expect(folderNameProblem(".hidden", [])).toBe(CLONE_DESCRIPTIONS.folder_invalid);
    expect(folderNameProblem("a/b", [])).toBe(CLONE_DESCRIPTIONS.folder_invalid);
    expect(folderNameProblem("notes", ["notes"])).toBe(CLONE_DESCRIPTIONS.folder_exists);
  });
});
