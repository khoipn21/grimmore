import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { cp, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as sleep } from "node:timers/promises";

const REQUEST_TIMEOUT_MS = 5_000;
const MAX_MESSAGE_BYTES = 4 * 1024 * 1024;

function parseArguments(argv) {
  const flags = new Map([
    ["--daemon", "daemon"],
    ["--launcher", "launcher"],
    ["--fixture-vault", "fixtureVault"],
    ["--workspace", "workspace"],
    ["--endpoint", "endpoint"],
  ]);
  const values = {};

  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const name = flags.get(flag);
    assert.ok(name !== undefined, "unsupported argument: " + flag);
    const value = argv[index + 1];
    assert.ok(value !== undefined && !value.startsWith("--"), "missing value for " + flag);
    assert.equal(values[name], undefined, "duplicate argument: " + flag);
    values[name] = value;
    index += 1;
  }

  for (const name of flags.values()) {
    const value = values[name];
    assert.ok(typeof value === "string" && value.length > 0, "missing " + name);
    assert.ok(isAbsolute(value), name + " must be an absolute path");
  }

  return values;
}

function framedJson(value) {
  const payload = Buffer.from(JSON.stringify(value));
  const frame = Buffer.allocUnsafe(4 + payload.byteLength);
  frame.writeUInt32BE(payload.byteLength, 0);
  payload.copy(frame, 4);
  return frame;
}

function firstFramedJson(buffer) {
  if (buffer.byteLength < 4) {
    return undefined;
  }
  const length = buffer.readUInt32BE(0);
  if (length === 0 || length > MAX_MESSAGE_BYTES) {
    throw new Error("launcher returned an invalid frame");
  }
  if (buffer.byteLength < 4 + length) {
    return undefined;
  }
  return JSON.parse(buffer.subarray(4, 4 + length).toString("utf8"));
}

function parseFramedJson(buffer) {
  let offset = 0;
  const messages = [];
  while (offset < buffer.byteLength) {
    if (buffer.byteLength - offset < 4) {
      throw new Error("launcher returned a truncated frame header");
    }
    const length = buffer.readUInt32BE(offset);
    offset += 4;
    if (
      length === 0 ||
      length > MAX_MESSAGE_BYTES ||
      buffer.byteLength - offset < length
    ) {
      throw new Error("launcher returned an invalid frame");
    }
    messages.push(JSON.parse(buffer.subarray(offset, offset + length).toString("utf8")));
    offset += length;
  }
  return messages;
}

function pluginRequest(id, method, params) {
  return {
    jsonrpc: "2.0",
    id,
    method,
    params,
    deadlineUnixMs: Date.now() + REQUEST_TIMEOUT_MS,
    vaultId: "reference",
    grantId: "local",
    scopeId: "vault",
  };
}

function startDaemon(config, vault, database) {
  const child = spawn(
    config.daemon,
    [
      "--database",
      database,
      "serve",
      "--vault-id",
      "reference",
      "--vault",
      vault,
      "--grant-id",
      "local",
      "--scope-id",
      "vault",
      "--endpoint",
      config.endpoint,
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return { child, stderr: () => stderr };
}

function waitForExit(child, description) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(description + " did not exit within 5000ms"));
    }, REQUEST_TIMEOUT_MS);
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal });
    });
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
}

async function stopDaemon(daemon, signal) {
  if (daemon === undefined) {
    return undefined;
  }
  if (daemon.child.exitCode === null && daemon.child.signalCode === null) {
    daemon.child.kill(signal);
  }
  return waitForExit(daemon.child, "installed companion");
}

function callLauncher(launcher, endpoint, request) {
  return new Promise((resolve, reject) => {
    const child = spawn(launcher, ["plugin-session", "--endpoint", endpoint], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    const stdout = [];
    let stderr = "";
    let settled = false;
    let response;
    let inputClosed = false;
    let timer;
    const fail = (error) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      reject(error);
    };

    timer = setTimeout(() => {
      fail(new Error("installed launcher did not return an IPC response within 5000ms"));
    }, REQUEST_TIMEOUT_MS);
    child.stdout.on("data", (chunk) => {
      stdout.push(chunk);
      try {
        if (response === undefined) {
          response = firstFramedJson(Buffer.concat(stdout));
          if (response !== undefined && !inputClosed) {
            inputClosed = true;
            child.stdin.end();
          }
        }
      } catch (error) {
        fail(error);
      }
    });
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", fail);
    child.stdin.on("error", fail);
    child.on("close", (code, signal) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      if (code !== 0) {
        reject(
          new Error(
            "installed launcher exited with " + (signal ?? "code " + code) + ": " + stderr,
          ),
        );
        return;
      }
      try {
        const messages = parseFramedJson(Buffer.concat(stdout));
        assert.equal(messages.length, 1, "launcher returns exactly one response");
        resolve(response ?? messages[0]);
      } catch (error) {
        reject(error);
      }
    });
    child.stdin.write(framedJson(request));
  });
}

async function waitForLauncherResponse(launcher, endpoint, request, predicate) {
  const deadline = Date.now() + REQUEST_TIMEOUT_MS;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const response = await callLauncher(launcher, endpoint, request());
      if (predicate(response)) {
        return response;
      }
      lastError = new Error("unexpected launcher response: " + JSON.stringify(response));
    } catch (error) {
      lastError = error;
    }
    await sleep(50);
  }
  throw lastError ?? new Error("installed launcher never received a response");
}

async function waitForAbsent(path, description) {
  const deadline = Date.now() + REQUEST_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      await stat(path);
    } catch (error) {
      if (error?.code === "ENOENT") {
        return;
      }
      throw error;
    }
    await sleep(25);
  }
  throw new Error(description + " remained after 5000ms");
}

function startMcp(config) {
  const child = spawn(
    config.daemon,
    [
      "mcp-stdio",
      "--vault-id",
      "reference",
      "--grant-id",
      "local",
      "--scope-id",
      "vault",
      "--endpoint",
      config.endpoint,
    ],
    { stdio: ["pipe", "pipe", "pipe"] },
  );
  let stderr = "";
  let terminalError;
  let closingInput = false;
  let closed = false;
  let exitStatus;
  const pending = new Map();
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });

  const recordTerminalError = (error) => {
    terminalError ??= error;
    for (const request of pending.values()) {
      clearTimeout(request.timer);
      request.reject(error);
    }
    pending.clear();
  };

  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  lines.on("line", (line) => {
    if (Buffer.byteLength(line, "utf8") > MAX_MESSAGE_BYTES) {
      recordTerminalError(new Error("MCP bridge returned an oversized JSON line"));
      return;
    }
    let response;
    try {
      response = JSON.parse(line);
    } catch (error) {
      recordTerminalError(error);
      return;
    }
    const request = pending.get(response.id);
    if (request === undefined) {
      return;
    }
    pending.delete(response.id);
    clearTimeout(request.timer);
    request.resolve(response);
  });
  child.on("error", recordTerminalError);
  child.on("close", (code, signal) => {
    closed = true;
    exitStatus = { code, signal };
    if (!closingInput || code !== 0 || signal !== null) {
      recordTerminalError(
        new Error(
          "installed MCP bridge exited with " + (signal ?? "code " + code) + ": " + stderr,
        ),
      );
    }
  });

  const waitForClose = () => {
    if (closed) {
      return Promise.resolve(exitStatus);
    }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new Error("installed MCP bridge did not exit within 5000ms"));
      }, REQUEST_TIMEOUT_MS);
      child.once("close", (code, signal) => {
        clearTimeout(timer);
        resolve({ code, signal });
      });
    });
  };

  const writeJsonLine = (message) =>
    new Promise((resolve, reject) => {
      child.stdin.write(JSON.stringify(message) + "\n", (error) => {
        if (error == null) {
          resolve();
        } else {
          reject(error);
        }
      });
    });

  return {
    child,
    async request(id, method, params) {
      assert.equal(pending.has(id), false, "MCP request identifiers must be unique");
      const response = new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          pending.delete(id);
          reject(new Error("MCP " + method + " request timed out"));
        }, REQUEST_TIMEOUT_MS);
        pending.set(id, { resolve, reject, timer });
      });
      try {
        await writeJsonLine({ jsonrpc: "2.0", id, method, params });
      } catch (error) {
        const request = pending.get(id);
        if (request !== undefined) {
          pending.delete(id);
          clearTimeout(request.timer);
          request.reject(error);
        }
      }
      return response;
    },
    notify(method, params) {
      return writeJsonLine({ jsonrpc: "2.0", method, params });
    },
    ensureRunning() {
      if (terminalError !== undefined) {
        throw terminalError;
      }
      if (child.exitCode !== null || child.signalCode !== null) {
        throw new Error("installed MCP bridge exited before the test closed its input");
      }
    },
    async close() {
      if (!closed && child.exitCode === null && child.signalCode === null) {
        closingInput = true;
        child.stdin.end();
      }
      try {
        const exit = await waitForClose();
        if (terminalError !== undefined) {
          throw terminalError;
        }
        assert.equal(exit.code, 0, "installed MCP bridge exits cleanly after stdin closes");
        assert.equal(exit.signal, null, "installed MCP bridge exits without a signal");
      } catch (error) {
        if (!closed) {
          child.kill("SIGKILL");
        }
        throw error;
      }
    },
  };
}

function mcpResult(response, id, description) {
  assert.equal(response.jsonrpc, "2.0", description + " uses JSON-RPC 2.0");
  assert.equal(response.id, id, description + " response ID matches its request");
  assert.equal(response.error, undefined, description + " did not return an MCP error");
  assert.equal(Object.hasOwn(response, "result"), true, description + " returned a result");
  return response.result;
}

async function exerciseInstalledMcp(config) {
  const mcp = startMcp(config);
  try {
    const initialized = mcpResult(
      await mcp.request(1, "initialize", {
        protocolVersion: "2025-11-25",
        capabilities: {},
        clientInfo: { name: "grimmore-phase-1-native-gate", version: "1.0.0" },
      }),
      1,
      "MCP initialization",
    );
    assert.equal(initialized.capabilities?.tools !== undefined, true, "MCP exposes tools");
    await mcp.notify("notifications/initialized", {});

    const tools = mcpResult(await mcp.request(2, "tools/list", {}), 2, "MCP tool list").tools;
    const names = tools.map((tool) => tool.name).sort();
    assert.deepEqual(names, ["grimmore_health", "grimmore_search_knowledge"]);
    assert.equal(
      tools.every((tool) => tool.annotations?.readOnlyHint === true),
      true,
      "installed MCP tools stay read-only",
    );

    const health = mcpResult(
      await mcp.request(3, "tools/call", { name: "grimmore_health", arguments: {} }),
      3,
      "MCP health query",
    ).structuredContent;
    assert.equal(health?.role, "mcp-readonly", "installed MCP bridge keeps its read-only role");

    const search = mcpResult(
      await mcp.request(4, "tools/call", {
        name: "grimmore_search_knowledge",
        arguments: { query: "context engineering", limit: 5 },
      }),
      4,
      "MCP knowledge query",
    ).structuredContent;
    assert.equal(
      search?.hits?.[0]?.path,
      "knowledge/ai/context-engineering.md",
      "installed MCP query returns the indexed fixture note",
    );
    await new Promise((resolve) => setImmediate(resolve));
    mcp.ensureRunning();
  } finally {
    await mcp.close();
  }
}

async function workspaceExists(path) {
  try {
    await stat(path);
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") {
      return false;
    }
    throw error;
  }
}

async function exerciseLifecycle(config) {
  assert.equal(await workspaceExists(config.workspace), false, "lifecycle workspace must be new");
  await mkdir(config.workspace, { recursive: true, mode: 0o700 });
  await mkdir(dirname(config.endpoint), { recursive: true, mode: 0o700 });
  const vault = join(config.workspace, "vault");
  const database = join(config.workspace, "operational.sqlite3");
  await cp(config.fixtureVault, vault, { recursive: true });

  let daemon;
  let requestId = 0;
  const waitForHealth = () =>
    waitForLauncherResponse(
      config.launcher,
      config.endpoint,
      () => pluginRequest(++requestId, "system.health", {}),
      (response) => response.result?.status === "ok" && response.result?.role === "plugin",
    );

  try {
    daemon = startDaemon(config, vault, database);
    await waitForHealth();
    const cleanStop = await stopDaemon(daemon, "SIGINT");
    assert.equal(cleanStop?.code, 0, "installed daemon accepts a clean stop");
    daemon = undefined;
    await waitForAbsent(config.endpoint, "cleanly stopped companion endpoint");

    daemon = startDaemon(config, vault, database);
    await waitForHealth();
    const crash = await stopDaemon(daemon, "SIGKILL");
    assert.equal(crash?.signal, "SIGKILL", "installed daemon crashes when forced");
    daemon = undefined;

    daemon = startDaemon(config, vault, database);
    await waitForHealth();
    const watchedNote = join(vault, "daily", "2026-07-13.md");
    const original = await readFile(watchedNote, "utf8");
    await writeFile(
      watchedNote,
      original + "\nInstalled lifecycle watcher recovery sentinel.\n",
      "utf8",
    );
    await waitForLauncherResponse(
      config.launcher,
      config.endpoint,
      () =>
        pluginRequest(++requestId, "knowledge.search", {
          query: "installed lifecycle watcher recovery sentinel",
          limit: 5,
        }),
      (response) =>
        response.result?.hits?.some((hit) => hit.path === "daily/2026-07-13.md") === true,
    );
    await exerciseInstalledMcp(config);

    const finalStop = await stopDaemon(daemon, "SIGINT");
    assert.equal(finalStop?.code, 0, "restarted daemon accepts a clean stop");
    daemon = undefined;
    await waitForAbsent(config.endpoint, "restarted companion endpoint");
  } finally {
    await stopDaemon(daemon, "SIGKILL").catch(() => {});
  }
}

async function main() {
  assert.ok(
    process.platform === "darwin" || process.platform === "linux",
    "installed lifecycle smoke requires POSIX signal semantics",
  );
  await exerciseLifecycle(parseArguments(process.argv.slice(2)));
  console.log("Installed lifecycle and MCP smoke passed.");
}

main().catch((error) => {
  console.error(error?.stack ?? error);
  process.exitCode = 1;
});
