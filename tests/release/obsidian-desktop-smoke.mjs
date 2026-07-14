import assert from "node:assert/strict";
/* global Buffer, WebSocket, clearTimeout, fetch, setTimeout */
import { spawn } from "node:child_process";
import { cp, copyFile, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { homedir, tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const repository = resolve(import.meta.dirname, "../..");
const pluginSource = join(repository, "apps", "obsidian-plugin");
const fixtureVault = join(repository, "tests", "fixtures", "vaults", "reference-vault");
const notePath = "daily/2026-07-13.md";
const appliedMarker = "Desktop Obsidian reviewed patch sentinel.";
const staleMarker = "Desktop Obsidian stale revision sentinel.";
const synchronousLoadMeasure = "grimmore-plugin-synchronous-load";
const synchronousLoadSamples = 20;
const synchronousLoadP95BudgetMs = 50;

function parseArguments(arguments_) {
  const values = new Map();
  const allowed = new Set([
    "--obsidian",
    "--daemon",
    "--launcher",
    "--flatpak-app",
    "--fixture-vault",
    "--workspace",
    "--report",
    "--disable-sandbox",
  ]);
  for (let index = 0; index < arguments_.length;) {
    const name = arguments_[index];
    if (!name?.startsWith("--")) {
      throw new Error(
        "usage: node tests/release/obsidian-desktop-smoke.mjs --obsidian <desktop-executable> --daemon <grimmored> --launcher <grimmore-launcher> [--flatpak-app <application-id>] [--fixture-vault <vault>] [--workspace <directory>] [--report <path>] [--disable-sandbox]",
      );
    }
    if (!allowed.has(name)) {
      throw new Error(`unknown argument: ${name}`);
    }
    if (values.has(name)) {
      throw new Error(`duplicate argument: ${name}`);
    }
    if (name === "--disable-sandbox") {
      values.set(name, true);
      index += 1;
      continue;
    }
    const value = arguments_[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(
        "usage: node tests/release/obsidian-desktop-smoke.mjs --obsidian <desktop-executable> --daemon <grimmored> --launcher <grimmore-launcher> [--flatpak-app <application-id>] [--fixture-vault <vault>] [--workspace <directory>] [--report <path>] [--disable-sandbox]",
      );
    }
    values.set(name, value);
    index += 2;
  }
  for (const name of ["--obsidian", "--daemon", "--launcher"]) {
    if (!values.has(name)) {
      throw new Error(`missing required argument: ${name}`);
    }
  }
  return {
    obsidian: resolve(values.get("--obsidian")),
    daemon: resolve(values.get("--daemon")),
    disableSandbox: values.get("--disable-sandbox") === true,
    flatpakApp: values.get("--flatpak-app"),
    launcher: resolve(values.get("--launcher")),
    fixtureVault: resolve(values.get("--fixture-vault") ?? fixtureVault),
    workspace: values.has("--workspace")
      ? resolve(values.get("--workspace"))
      : values.has("--flatpak-app")
        ? join(homedir(), ".cache", "grimmore", `obsidian-desktop-${process.pid}`)
        : join(tmpdir(), `grimmore-obsidian-desktop-${process.pid}`),
    report: values.has("--report") ? resolve(values.get("--report")) : undefined,
  };
}

async function assertFile(path, description) {
  const metadata = await stat(path).catch(() => undefined);
  assert(metadata?.isFile(), `${description} is not a file: ${path}`);
}

function spawnProcess(command, arguments_, options) {
  const child = spawn(command, arguments_, {
    cwd: options.cwd,
    env: options.env,
    stdio: ["ignore", "ignore", "pipe"],
    windowsHide: true,
    detached: process.platform !== "win32",
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return { child, stderr: () => stderr };
}

function waitForExit(child, description, timeoutMs = 10_000) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve_, reject) => {
    const timer = setTimeout(() => reject(new Error(`${description} did not exit within ${timeoutMs}ms`)), timeoutMs);
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      resolve_({ code, signal });
    });
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
}

async function stopProcess(process_, description) {
  if (process_ === undefined) return;
  const { child } = process_;
  if (child.exitCode === null && child.signalCode === null) await terminateProcessTree(child, "SIGTERM");
  try {
    await waitForExit(child, description, 5_000);
  } catch {
    if (child.exitCode === null && child.signalCode === null) await terminateProcessTree(child, "SIGKILL");
    await waitForExit(child, description, 5_000);
  }
}

async function terminateProcessTree(child, signal) {
  if (process.platform !== "win32") {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch {
      child.kill(signal);
      return;
    }
  }
  await new Promise((resolve_, reject) => {
    const taskkill = spawn("taskkill", ["/pid", String(child.pid), "/t", "/f"], {
      stdio: "ignore",
      windowsHide: true,
    });
    taskkill.once("error", reject);
    taskkill.once("close", (code) => {
      if (code === 0 || child.exitCode !== null || child.signalCode !== null) resolve_();
      else reject(new Error(`taskkill failed with exit code ${code}`));
    });
  });
}

function framedJson(value) {
  const payload = Buffer.from(JSON.stringify(value));
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32BE(payload.byteLength);
  return Buffer.concat([header, payload]);
}

function firstFramedJson(buffer) {
  if (buffer.byteLength < 4) return undefined;
  const length = buffer.readUInt32BE(0);
  if (length === 0 || length > 4 * 1024 * 1024) {
    throw new Error("launcher returned an invalid health frame");
  }
  if (buffer.byteLength < length + 4) return undefined;
  return JSON.parse(buffer.subarray(4, length + 4).toString("utf8"));
}

function launcherHealth(launcher, environment, cwd) {
  return new Promise((resolve_, reject) => {
    const child = spawn(launcher, ["plugin-session"], {
      cwd,
      env: environment,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdout = [];
    let stderr = "";
    let response;
    let inputClosed = false;
    let settled = false;
    const fail = (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill();
      reject(error);
    };
    const timer = setTimeout(() => fail(new Error("launcher health response timed out")), 5_000);
    child.stdout.on("data", (chunk) => {
      stdout.push(chunk);
      try {
        response ??= firstFramedJson(Buffer.concat(stdout));
        if (response !== undefined && !inputClosed) {
          inputClosed = true;
          child.stdin.end();
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
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (code !== 0) {
        reject(new Error(`launcher health exited with ${signal ?? `code ${code}`}: ${stderr}`));
      } else if (response === undefined) {
        reject(new Error("launcher closed without a health response"));
      } else {
        resolve_(response);
      }
    });
    child.stdin.write(
      framedJson({
        jsonrpc: "2.0",
        id: 1,
        method: "system.health",
        params: {},
        deadlineUnixMs: Date.now() + 5_000,
        vaultId: "reference",
        grantId: "local",
        scopeId: "vault",
      }),
      (error) => {
        if (error !== null && error !== undefined) fail(error);
      },
    );
  });
}

async function unusedPort() {
  const server = createServer();
  await new Promise((resolve_, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve_);
  });
  const address = server.address();
  assert(address !== null && typeof address !== "string", "allocate a TCP debug port");
  const { port } = address;
  await new Promise((resolve_, reject) => server.close((error) => (error === undefined ? resolve_() : reject(error))));
  return port;
}

async function waitFor(fetch_, predicate, description, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await fetch_();
      if (predicate(value)) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(100);
  }
  throw new Error(`${description} did not become ready within ${timeoutMs}ms${lastError === undefined ? "" : `: ${lastError.message}`}`);
}

async function debugTarget(port, process_) {
  let targets;
  try {
    targets = await waitFor(
      async () => {
        const response = await fetch(`http://127.0.0.1:${port}/json/list`);
        if (!response.ok) throw new Error(`remote debugger returned HTTP ${response.status}`);
        return response.json();
      },
      (value) => Array.isArray(value) && value.some((target) => target?.type === "page" && target?.url?.startsWith("app://obsidian.md/") && typeof target.webSocketDebuggerUrl === "string"),
      "Obsidian remote debugger",
    );
  } catch (error) {
    const exit = process_.child.exitCode === null && process_.child.signalCode === null
      ? "still running"
      : process_.child.signalCode === null
        ? `exited with code ${process_.child.exitCode}`
        : `exited from signal ${process_.child.signalCode}`;
    const stderr = process_.stderr().trim();
    throw new Error(`${error.message}; Obsidian ${exit}${stderr.length === 0 ? "" : `; stderr: ${stderr}`}`);
  }
  return targets.find((target) => target?.type === "page" && target?.url?.startsWith("app://obsidian.md/") && typeof target.webSocketDebuggerUrl === "string");
}

class DevToolsSession {
  #socket;
  #nextId = 1;
  #pending = new Map();

  static async connect(url) {
    const session = new DevToolsSession();
    await session.#open(url);
    return session;
  }

  async #open(url) {
    this.#socket = new WebSocket(url);
    this.#socket.addEventListener("message", (event) => {
      void this.#acceptMessage(event.data);
    });
    this.#socket.addEventListener("close", () => this.#rejectPending(new Error("Obsidian DevTools connection closed")));
    this.#socket.addEventListener("error", () => this.#rejectPending(new Error("Obsidian DevTools connection failed")));
    await new Promise((resolve_, reject) => {
      this.#socket.addEventListener("open", resolve_, { once: true });
      this.#socket.addEventListener("error", reject, { once: true });
    });
  }

  #rejectPending(error) {
    for (const { reject, timer } of this.#pending.values()) {
      clearTimeout(timer);
      reject(error);
    }
    this.#pending.clear();
  }

  async #acceptMessage(data) {
    try {
      const payload = typeof data === "string"
        ? data
        : data instanceof ArrayBuffer
          ? new TextDecoder().decode(data)
          : await data.text();
      const message = JSON.parse(payload);
      const pending = this.#pending.get(message.id);
      if (pending !== undefined) {
        this.#pending.delete(message.id);
        clearTimeout(pending.timer);
        pending.resolve(message);
      }
    } catch (error) {
      this.#rejectPending(new Error(`Obsidian DevTools returned an invalid message: ${error.message}`));
    }
  }

  send(method, params = {}) {
    const id = this.#nextId++;
    return new Promise((resolve_, reject) => {
      const timer = setTimeout(() => {
        if (this.#pending.delete(id)) reject(new Error(`Obsidian DevTools ${method} request timed out`));
      }, 15_000);
      this.#pending.set(id, { resolve: resolve_, reject, timer });
      this.#socket.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const response = await this.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true, userGesture: true });
    if (response.error !== undefined) throw new Error(`DevTools evaluation failed: ${response.error.message}`);
    if (response.result.exceptionDetails !== undefined) {
      throw new Error(`Obsidian renderer evaluation failed: ${response.result.exceptionDetails.exception?.description ?? response.result.exceptionDetails.text}`);
    }
    return response.result.result.value;
  }

  close() {
    this.#socket?.close();
  }
}

async function waitForRenderer(session, expression, description) {
  return waitFor(() => session.evaluate(expression), Boolean, description);
}

function percentile95(samples) {
  const sorted = [...samples].sort((left, right) => left - right);
  return sorted[Math.ceil(sorted.length * 0.95) - 1];
}

function sampleVariance(samples) {
  const mean = samples.reduce((total, sample) => total + sample, 0) / samples.length;
  return samples.reduce((total, sample) => total + (sample - mean) ** 2, 0) /
    (samples.length - 1);
}

async function writeReport(path, value) {
  if (path === undefined) return;
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
}

async function measureSynchronousPluginLoad(session) {
  const sample = await waitFor(
    () => session.evaluate(`performance.getEntriesByName(${JSON.stringify(synchronousLoadMeasure)}).at(-1)?.duration`),
    (value) => typeof value === "number" && Number.isFinite(value) && value >= 0,
    "a synchronous Grimmore plugin-load measurement",
  );
  return sample;
}

async function enableTestCommunityPlugins(session) {
  return session.evaluate(`(async () => {
    localStorage.setItem("enable-plugin-" + app.appId, "true");
    await app.plugins.initialize();
    return app.plugins.isEnabled();
  })()`);
}

function openAndStartReview() {
  return `(async () => {
    const file = app.vault.getAbstractFileByPath(${JSON.stringify(notePath)});
    if (file === null || file.path !== ${JSON.stringify(notePath)}) throw new Error("fixture note is unavailable");
    await app.workspace.getLeaf(false).openFile(file);
    const started = app.commands.executeCommandById("grimmore:review-active-note-replacement");
    if (!started) throw new Error("Grimmore replacement-review command was unavailable");
    return true;
  })()`;
}

function submitReplacement(replacement) {
  return `(() => {
    const input = document.querySelector(".grimmore-replacement-input");
    if (!(input instanceof HTMLTextAreaElement)) return false;
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
    setter?.call(input, ${JSON.stringify(replacement)});
    input.dispatchEvent(new Event("input", { bubbles: true }));
    const button = [...document.querySelectorAll("button")].find((candidate) => candidate.textContent?.trim() === "Review diff");
    if (!(button instanceof HTMLButtonElement)) return false;
    button.click();
    return true;
  })()`;
}

const applyReviewedPatch = `(() => {
  const button = [...document.querySelectorAll("button")].find((candidate) => candidate.textContent?.trim() === "Apply reviewed patch");
  if (!(button instanceof HTMLButtonElement)) return false;
  button.click();
  return true;
})()`;

async function prepareVault(options) {
  const vault = join(options.workspace, "vault");
  const plugin = join(vault, ".obsidian", "plugins", "grimmore");
  await cp(options.fixtureVault, vault, { recursive: true });
  await mkdir(plugin, { recursive: true });
  for (const name of ["manifest.json", "main.js", "styles.css"]) {
    await copyFile(join(pluginSource, name), join(plugin, name));
  }
  await writeFile(join(vault, ".obsidian", "community-plugins.json"), '["grimmore"]\n');
  await writeFile(join(plugin, "data.json"), '{"vaultId":"reference"}\n');
  return vault;
}

async function prepareObsidianProfile(profile, vault) {
  await mkdir(profile, { recursive: true, mode: 0o700 });
  await writeFile(
    join(profile, "obsidian.json"),
    `${JSON.stringify({ vaults: { reference: { path: vault, ts: Date.now(), open: true } } })}\n`,
    { mode: 0o600 },
  );
}

function startObsidian(options, vault, port, profile, environment) {
  const arguments_ = [
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
  ];
  if (options.disableSandbox) arguments_.push("--no-sandbox");
  if (options.flatpakApp === undefined) {
    return spawnProcess(options.obsidian, arguments_, {
      cwd: options.workspace,
      env: environment,
    });
  }
  if (typeof environment.XDG_RUNTIME_DIR !== "string" || environment.XDG_RUNTIME_DIR.length === 0) {
    throw new Error("Flatpak desktop smoke requires XDG_RUNTIME_DIR for the companion socket mount");
  }
  return spawnProcess(
    options.obsidian,
    [
      "run",
      `--filesystem=${join(environment.XDG_RUNTIME_DIR, "grimmore")}`,
      `--env=XDG_RUNTIME_DIR=${environment.XDG_RUNTIME_DIR}`,
      `--env=PATH=${environment.PATH}`,
      "--command=obsidian.sh",
      options.flatpakApp,
      ...arguments_,
    ],
    { cwd: options.workspace, env: environment },
  );
}

function inheritedPath() {
  return Object.entries(process.env).find(([name]) => name.toLowerCase() === "path")?.[1] ?? "";
}

async function collectSynchronousPluginLoadSamples(options, vault, environment) {
  const samples = [];
  for (let index = 0; index < synchronousLoadSamples; index += 1) {
    const profile = join(options.workspace, `obsidian-metric-profile-${index + 1}`);
    const port = await unusedPort();
    let obsidian;
    let session;
    try {
      await prepareObsidianProfile(profile, vault);
      obsidian = startObsidian(options, vault, port, profile, environment);
      const target = await debugTarget(port, obsidian);
      session = await DevToolsSession.connect(target.webSocketDebuggerUrl);
      await waitForRenderer(session, 'typeof app !== "undefined" && app.plugins !== undefined', "the Obsidian plugin manager for timing");
      assert.equal(await enableTestCommunityPlugins(session), true, "enable community plugins in the isolated timing profile");
      await waitForRenderer(session, 'Boolean(globalThis.app?.plugins?.getPlugin("grimmore"))', "the production Grimmore plugin to load for timing");
      samples.push(await measureSynchronousPluginLoad(session));
    } finally {
      session?.close();
      await stopProcess(obsidian, `Obsidian desktop timing sample ${index + 1}`);
    }
  }
  return {
    p95: percentile95(samples),
    samples,
    variance: sampleVariance(samples),
  };
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  await assertFile(options.obsidian, "Obsidian desktop executable");
  await assertFile(options.daemon, "grimmored executable");
  await assertFile(options.launcher, "grimmore-launcher executable");
  for (const name of ["main.js", "manifest.json", "styles.css"]) await assertFile(join(pluginSource, name), `production plugin ${name}`);
  await assertFile(join(options.fixtureVault, ".obsidian", "app.json"), "real fixture vault marker");
  await rm(options.workspace, { recursive: true, force: true });
  await mkdir(options.workspace, { recursive: true, mode: 0o700 });

  let daemon;
  let obsidian;
  let session;
  let result;
  try {
    const vault = await prepareVault(options);
    const runtime = join(options.workspace, "runtime");
    const profile = join(options.workspace, "obsidian-profile");
    await mkdir(runtime, { recursive: true, mode: 0o700 });
    await prepareObsidianProfile(profile, vault);
    const port = await unusedPort();
    const pathDelimiter = process.platform === "win32" ? ";" : ":";
    const environment = Object.fromEntries(
      Object.entries(process.env).filter(([name]) => name.toLowerCase() !== "path"),
    );
    environment[process.platform === "win32" ? "Path" : "PATH"] = `${dirname(options.launcher)}${pathDelimiter}/app/bin${pathDelimiter}/usr/bin${pathDelimiter}${inheritedPath()}`;
    if (process.platform !== "win32" && options.flatpakApp === undefined) {
      environment.XDG_RUNTIME_DIR = runtime;
    }
    daemon = spawnProcess(options.daemon, ["--database", join(options.workspace, "grimmore.sqlite3"), "serve", "--vault-id", "reference", "--vault", vault], { cwd: options.workspace, env: environment });
    await waitFor(
      () => launcherHealth(options.launcher, environment, options.workspace),
      (response) => response?.result?.status === "ok" && response.result.role === "plugin",
      "the native companion through its stable launcher",
    ).catch((error) => {
      throw new Error(`${error.message}; grimmored stderr: ${daemon.stderr()}`);
    });
    obsidian = startObsidian(options, vault, port, profile, environment);
    const target = await debugTarget(port, obsidian);
    session = await DevToolsSession.connect(target.webSocketDebuggerUrl);
    await waitForRenderer(session, 'typeof app !== "undefined" && app.plugins !== undefined', "the Obsidian plugin manager");
    assert.equal(await enableTestCommunityPlugins(session), true, "enable community plugins in the isolated desktop-smoke profile");
    await waitForRenderer(session, 'Boolean(globalThis.app?.plugins?.getPlugin("grimmore"))', "the production Grimmore plugin to load in real Obsidian");
    await waitForRenderer(session, 'Boolean(app.workspace?.rootSplit?.containerEl?.isConnected)', "the real Obsidian workspace layout");
    await waitForRenderer(session, `Boolean(app.vault.getAbstractFileByPath(${JSON.stringify(notePath)}))`, "the fixture note to load in real Obsidian");
    const obsidianVersion = await session.evaluate(
      'typeof app.appVersion === "string" ? app.appVersion : document.title.match(/Obsidian ([0-9][^\\s]*)$/)?.[1] ?? null',
    );
    assert(
      typeof obsidianVersion === "string" && obsidianVersion.length > 0,
      "Obsidian exposes a non-empty desktop version",
    );
    const initial = await readFile(join(vault, notePath), "utf8");
    const approvedReplacement = `${initial}\n${appliedMarker}\n`;
    assert.equal(await session.evaluate(openAndStartReview()), true, "start patch review");
    await waitForRenderer(session, 'document.querySelector(".grimmore-replacement-input") instanceof HTMLTextAreaElement', "replacement input modal");
    assert.equal(await session.evaluate(submitReplacement(approvedReplacement)), true);
    await waitForRenderer(session, `document.querySelector(".grimmore-diff-preview")?.textContent?.includes(${JSON.stringify(appliedMarker)})`, "reviewed unified diff");
    assert.equal(await session.evaluate(applyReviewedPatch), true, "approve reviewed patch");
    await waitFor(() => readFile(join(vault, notePath), "utf8"), (content) => content === approvedReplacement, "Obsidian Vault.process approved write");

    const staleReplacement = `${approvedReplacement}\nThis stale replacement must never be written.\n`;
    assert.equal(await session.evaluate(openAndStartReview()), true, "start stale patch review");
    await waitForRenderer(session, 'document.querySelector(".grimmore-replacement-input") instanceof HTMLTextAreaElement', "replacement input modal for stale write");
    assert.equal(await session.evaluate(submitReplacement(staleReplacement)), true);
    await waitForRenderer(session, 'document.querySelector(".grimmore-diff-preview") instanceof HTMLPreElement', "stale patch review diff");
    const externallyChanged = `${approvedReplacement}\n${staleMarker}\n`;
    await writeFile(join(vault, notePath), externallyChanged, "utf8");
    await waitForRenderer(session, `(async () => {
      const file = app.vault.getAbstractFileByPath(${JSON.stringify(notePath)});
      return file !== null && (await app.vault.read(file)).includes(${JSON.stringify(staleMarker)});
    })()`, "Obsidian external-file reconciliation before stale approval");
    assert.equal(await session.evaluate(applyReviewedPatch), true, "attempt stale reviewed patch");
    await waitFor(() => readFile(join(vault, notePath), "utf8"), (content) => content === externallyChanged, "stale-write rejection preserving the external content");
    await waitForRenderer(session, 'document.body.textContent?.includes("The note changed after review. No content was written.")', "stale-revision notice");
    session.close();
    session = undefined;
    await stopProcess(obsidian, "Obsidian desktop");
    obsidian = undefined;

    const pluginLoad = await collectSynchronousPluginLoadSamples(options, vault, environment);
    result = {
      pluginSynchronousLoad: {
        budgetMs: synchronousLoadP95BudgetMs,
        measurement: "synchronous onload prefix through settings-hydration promise creation on a fresh real-Obsidian process",
        obsidianVersion,
        p95Ms: pluginLoad.p95,
        percentileMethod: "nearest-rank",
        retries: 0,
        samplesMs: pluginLoad.samples,
        sampleVarianceMs2: pluginLoad.variance,
        warmupSamples: 0,
      },
      status: "measured",
    };
    await writeReport(options.report, result);
    assert(pluginLoad.p95 <= synchronousLoadP95BudgetMs, `plugin synchronous-load p95 ${pluginLoad.p95.toFixed(3)}ms exceeds ${synchronousLoadP95BudgetMs}ms`);
    result.status = "passed";
    await writeReport(options.report, result);
    console.log(`real Obsidian desktop smoke passed (${basename(options.obsidian)}; production plugin bundle; plugin synchronous-load p95 ${pluginLoad.p95.toFixed(3)}ms; approved and stale reviewed writes)`);
  } catch (error) {
    if (result !== undefined) {
      result.failure = error instanceof Error ? error.message : String(error);
      result.status = "failed";
      await writeReport(options.report, result);
    }
    throw error;
  } finally {
    session?.close();
    await stopProcess(obsidian, "Obsidian desktop").catch(() => undefined);
    await stopProcess(daemon, "grimmored").catch(() => undefined);
  }
}

await main();
