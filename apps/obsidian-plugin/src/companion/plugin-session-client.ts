import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";

import {
  MAX_FRAME_BYTES,
  PROTOCOL_VERSION,
  type HealthResult,
  type JsonRpcFailure,
  type JsonRpcRequest,
  type JsonRpcResponse,
  type JsonRpcSuccess,
  type PatchProposal,
  type ProposeNoteReplacementParams,
} from "@grimmore/protocol";

const MAX_INFLIGHT_REQUESTS = 32;
const REQUEST_WINDOW_MS = 5_000;
const RESPONSE_TIMEOUT_MS = 6_000;
const LAUNCHER_COMMAND = "grimmore-launcher";
const UTF8_DECODER = new TextDecoder("utf-8", { fatal: true });

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
}

export interface PluginSessionOptions {
  vaultId: string;
  grantId: string;
  scopeId: string;
}

export class CompanionUnavailableError extends Error {
  public constructor() {
    super("The local Grimmore companion is unavailable.");
    this.name = "CompanionUnavailableError";
  }
}

export class CompanionRequestError extends Error {
  public readonly code: number;

  public constructor(failure: JsonRpcFailure) {
    super(failure.error.message);
    this.name = "CompanionRequestError";
    this.code = failure.error.code;
  }
}

export class PluginSessionClient {
  readonly #options: PluginSessionOptions;
  readonly #pending = new Map<number, PendingRequest>();
  #child: ChildProcessWithoutNullStreams | undefined;
  #starting: Promise<void> | undefined;
  #stdoutBuffer = Buffer.alloc(0);
  #nextRequestId = 1;

  public constructor(options: PluginSessionOptions) {
    this.#options = options;
  }

  public async health(): Promise<HealthResult> {
    return parseHealthResult(await this.#request("system.health", {}));
  }

  public async proposeNoteReplacement(
    params: ProposeNoteReplacementParams,
  ): Promise<PatchProposal> {
    return parsePatchProposal(
      await this.#request("knowledge.proposeNoteReplacement", params),
    );
  }

  public close(): void {
    this.#failConnection(new CompanionUnavailableError(), true);
  }

  async #request(method: string, params: unknown): Promise<unknown> {
    await this.#ensureStarted();
    const child = this.#child;
    if (child?.exitCode !== null || !child.stdin.writable) {
      throw new CompanionUnavailableError();
    }
    if (this.#pending.size >= MAX_INFLIGHT_REQUESTS) {
      throw new CompanionUnavailableError();
    }
    const id = this.#allocateRequestId();
    const request: JsonRpcRequest = {
      jsonrpc: "2.0",
      id,
      method,
      params,
      deadlineUnixMs: Date.now() + REQUEST_WINDOW_MS,
      vaultId: this.#options.vaultId,
      grantId: this.#options.grantId,
      scopeId: this.#options.scopeId,
    };
    const payload = Buffer.from(JSON.stringify(request), "utf8");
    if (payload.byteLength === 0 || payload.byteLength > MAX_FRAME_BYTES) {
      throw new CompanionUnavailableError();
    }
    const frame = Buffer.allocUnsafe(4 + payload.byteLength);
    frame.writeUInt32BE(payload.byteLength, 0);
    payload.copy(frame, 4);

    const response = new Promise<unknown>((resolve, reject) => {
      const requestTimeout = setTimeout(() => {
        this.#failConnection(new CompanionUnavailableError(), true);
      }, RESPONSE_TIMEOUT_MS);
      this.#pending.set(id, { resolve, reject, timeout: requestTimeout });
    });
    try {
      await new Promise<void>((resolve, reject) => {
        child.stdin.write(frame, (error) => {
          if (error === null || error === undefined) {
            resolve();
          } else {
            reject(error);
          }
        });
      });
    } catch {
      this.#failConnection(new CompanionUnavailableError(), true);
    }
    return response;
  }

  async #ensureStarted(): Promise<void> {
    if (this.#child !== undefined && this.#child.exitCode === null) {
      return;
    }
    this.#starting ??= this.#start();
    try {
      await this.#starting;
    } finally {
      this.#starting = undefined;
    }
  }

  async #start(): Promise<void> {
    const child = spawn(LAUNCHER_COMMAND, ["plugin-session"], {
      shell: false,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.#child = child;
    child.stdout.on("data", (chunk: Buffer) => {
      if (this.#child === child) {
        this.#acceptStdout(chunk);
      }
    });
    child.stderr.resume();
    child.on("close", () => {
      if (this.#child === child) {
        this.#failConnection(new CompanionUnavailableError(), false);
      }
    });
    child.on("error", () => {
      if (this.#child === child) {
        this.#failConnection(new CompanionUnavailableError(), false);
      }
    });
    await new Promise<void>((resolve, reject) => {
      child.once("spawn", resolve);
      child.once("error", () => {
        reject(new CompanionUnavailableError());
      });
    });
  }

  #acceptStdout(chunk: Buffer): void {
    const maximumBufferedBytes =
      (MAX_FRAME_BYTES + 4) * Math.max(1, this.#pending.size);
    if (this.#stdoutBuffer.byteLength + chunk.byteLength > maximumBufferedBytes) {
      this.#failConnection(new CompanionUnavailableError(), true);
      return;
    }
    this.#stdoutBuffer = Buffer.concat([this.#stdoutBuffer, chunk]);
    while (this.#stdoutBuffer.byteLength >= 4) {
      const frameLength = this.#stdoutBuffer.readUInt32BE(0);
      if (frameLength === 0 || frameLength > MAX_FRAME_BYTES) {
        this.#failConnection(new CompanionUnavailableError(), true);
        return;
      }
      if (this.#stdoutBuffer.byteLength < 4 + frameLength) {
        return;
      }
      const payload = this.#stdoutBuffer.subarray(4, 4 + frameLength);
      this.#stdoutBuffer = this.#stdoutBuffer.subarray(4 + frameLength);
      let parsed: unknown;
      try {
        parsed = JSON.parse(UTF8_DECODER.decode(payload)) as unknown;
      } catch {
        this.#failConnection(new CompanionUnavailableError(), true);
        return;
      }
      const response = parseJsonRpcResponse(parsed);
      const responseId = response?.id;
      if (response === undefined || typeof responseId !== "number") {
        this.#failConnection(new CompanionUnavailableError(), true);
        return;
      }
      const pending = this.#pending.get(responseId);
      if (pending === undefined) {
        this.#failConnection(new CompanionUnavailableError(), true);
        return;
      }
      clearTimeout(pending.timeout);
      this.#pending.delete(responseId);
      if ("error" in response) {
        pending.reject(new CompanionRequestError(response));
      } else {
        pending.resolve(response.result);
      }
    }
  }

  #allocateRequestId(): number {
    const id = this.#nextRequestId;
    this.#nextRequestId =
      id >= Number.MAX_SAFE_INTEGER ? 1 : this.#nextRequestId + 1;
    return id;
  }

  #failConnection(error: Error, terminate: boolean): void {
    const child = this.#child;
    this.#child = undefined;
    this.#stdoutBuffer = Buffer.alloc(0);
    for (const pending of this.#pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.#pending.clear();
    if (terminate && child?.exitCode === null) {
      child.kill();
    }
  }
}

function parseJsonRpcResponse(value: unknown): JsonRpcResponse | undefined {
  if (!isRecord(value) || value.jsonrpc !== "2.0") {
    return undefined;
  }
  if (
    typeof value.id === "number" &&
    Number.isSafeInteger(value.id) &&
    "result" in value &&
    !("error" in value)
  ) {
    return value as unknown as JsonRpcSuccess;
  }
  if (
    (value.id === null ||
      (typeof value.id === "number" && Number.isSafeInteger(value.id))) &&
    isRecord(value.error) &&
    typeof value.error.code === "number" &&
    typeof value.error.message === "string" &&
    !("result" in value)
  ) {
    return value as unknown as JsonRpcFailure;
  }
  return undefined;
}

function parseHealthResult(value: unknown): HealthResult {
  if (
    !isRecord(value) ||
    value.status !== "ok" ||
    typeof value.productVersion !== "string" ||
    value.productVersion.length === 0 ||
    value.protocolVersion !== PROTOCOL_VERSION ||
    value.role !== "plugin"
  ) {
    throw new CompanionUnavailableError();
  }
  return value as unknown as HealthResult;
}

function parsePatchProposal(value: unknown): PatchProposal {
  if (
    !isRecord(value) ||
    typeof value.path !== "string" ||
    typeof value.expectedRevision !== "string" ||
    typeof value.proposedRevision !== "string" ||
    typeof value.replacement !== "string"
  ) {
    throw new CompanionUnavailableError();
  }
  return value as unknown as PatchProposal;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
