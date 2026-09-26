import { spawn } from "node:child_process";

import { ProcessResult } from "./protocol";

export function spawnProcess(
  command: string,
  args: string[],
  cwd: string,
): Promise<ProcessResult> {
  return new Promise((resolve) => {
    let stdout = "";
    let stderr = "";
    let settled = false;
    const finish = (result: ProcessResult) => {
      if (settled) return;
      settled = true;
      resolve(result);
    };

    const child = spawn(command, args, { cwd, shell: false });
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });
    child.on("error", (err: NodeJS.ErrnoException) => {
      finish({
        stdout,
        stderr,
        exitCode: null,
        notFound: err.code === "ENOENT",
      });
    });
    child.on("close", (code) => {
      finish({
        stdout,
        stderr,
        exitCode: code,
        notFound: false,
      });
    });
  });
}
