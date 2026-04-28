import { spawn } from "node:child_process";

export interface AftRequest {
  id: string;
  command: string;
  [key: string]: unknown;
}

export interface AftResponse {
  id: string;
  success: boolean;
  code?: string;
  message?: string;
  [key: string]: unknown;
}

export async function sendAftRequest(
  binaryPath: string,
  request: AftRequest,
): Promise<AftResponse> {
  const responses = await sendAftRequests(binaryPath, [request]);
  const response = responses[0];
  if (!response) throw new Error("aft exited before responding");
  return response;
}

export async function sendAftRequests(
  binaryPath: string,
  requests: AftRequest[],
): Promise<AftResponse[]> {
  return new Promise((resolve, reject) => {
    const child = spawn(binaryPath, [], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    const responses: AftResponse[] = [];
    let stdout = "";
    let stderr = "";
    let settled = false;

    const finish = (fn: () => void): void => {
      if (settled) return;
      settled = true;
      child.kill();
      fn();
    };

    const handleLine = (line: string): void => {
      if (!line) return;
      try {
        responses.push(JSON.parse(line) as AftResponse);
      } catch (error) {
        finish(() => reject(error));
        return;
      }
      if (responses.length === requests.length) {
        finish(() => resolve(responses));
      }
    };

    child.stdout.setEncoding("utf-8");
    child.stdout.on("data", (chunk: string) => {
      stdout += chunk;
      while (true) {
        const newline = stdout.indexOf("\n");
        if (newline === -1) break;
        const line = stdout.slice(0, newline).trim();
        stdout = stdout.slice(newline + 1);
        handleLine(line);
        if (settled) break;
      }
    });

    child.stderr.setEncoding("utf-8");
    child.stderr.on("data", (chunk: string) => {
      stderr += chunk;
    });

    child.on("error", (error) => {
      finish(() => reject(error));
    });

    child.on("exit", (code) => {
      if (settled) return;
      finish(() =>
        reject(new Error(`aft exited before responding (code ${code}): ${stderr.trim()}`)),
      );
    });

    for (const request of requests) {
      child.stdin.write(`${JSON.stringify(request)}\n`);
    }
    child.stdin.end();
  });
}
