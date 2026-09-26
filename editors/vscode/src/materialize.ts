import { randomBytes } from "node:crypto";
import { rm, writeFile } from "node:fs/promises";
import path from "node:path";

export interface MaterializedSource {
  path: string;
  cleanup: () => Promise<void>;
}

export async function materializeSource(input: {
  directory: string;
  savedPath: string | null;
  dirty: boolean;
  text: string;
}): Promise<MaterializedSource> {
  if (!input.dirty && input.savedPath) {
    return { path: input.savedPath, cleanup: async () => {} };
  }
  const name = `.sal-editor-${process.pid}-${randomBytes(6).toString("hex")}.sal`;
  const filePath = path.join(input.directory, name);
  await writeFile(filePath, input.text, "utf8");
  return {
    path: filePath,
    cleanup: async () => {
      await rm(filePath, { force: true });
    },
  };
}
