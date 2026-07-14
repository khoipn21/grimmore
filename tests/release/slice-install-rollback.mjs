import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmod,
  cp,
  copyFile,
  mkdtemp,
  mkdir,
  readFile,
  readlink,
  rm,
  stat,
  symlink,
  unlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

const repository = resolve(import.meta.dirname, "../..");
const installer = join(repository, "installers/linux/install.sh");
const rollback = join(repository, "installers/linux/rollback.sh");
const manifestBuilder = join(repository, "release/create-slice-manifest.mjs");
const installedLifecycle = join(repository, "tests/release/installed-lifecycle.mjs");
const referenceVault = join(repository, "tests/fixtures/vaults/reference-vault");
const targetByArchitecture = {
  arm64: "linux-arm64-gnu",
  x64: "linux-x64-gnu",
};
const target = targetByArchitecture[process.arch];
const signer = "Grimmore phase one test <phase-one@example.invalid>";

function run(command, arguments_, options = {}) {
  return new Promise((resolve_, reject) => {
    const child = spawn(command, arguments_, {
      cwd: options.cwd ?? repository,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => {
      stdout += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("error", reject);
    child.on("close", (code, signal) => {
      if (code === 0) {
        resolve_({ stdout, stderr });
        return;
      }
      reject(
        new Error(
          `${command} ${arguments_.join(" ")} failed with ${
            signal ?? `exit ${code}`
          }\n${stderr}`,
        ),
      );
    });
  });
}

function framedJson(value) {
  const payload = Buffer.from(JSON.stringify(value));
  const header = Buffer.allocUnsafe(4);
  header.writeUInt32BE(payload.byteLength);
  return Buffer.concat([header, payload]);
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
    if (length === 0 || length > 4 * 1024 * 1024 || buffer.byteLength - offset < length) {
      throw new Error("launcher returned an invalid frame");
    }
    messages.push(JSON.parse(buffer.subarray(offset, offset + length).toString("utf8")));
    offset += length;
  }
  return messages;
}

function firstFramedJson(buffer) {
  if (buffer.byteLength < 4) {
    return undefined;
  }
  const length = buffer.readUInt32BE(0);
  if (length === 0 || length > 4 * 1024 * 1024) {
    throw new Error("launcher returned an invalid frame");
  }
  if (buffer.byteLength < 4 + length) {
    return undefined;
  }
  return JSON.parse(buffer.subarray(4, 4 + length).toString("utf8"));
}

function pluginRequest(id, method, params) {
  return {
    jsonrpc: "2.0",
    id,
    method,
    params,
    deadlineUnixMs: Date.now() + 5_000,
    vaultId: "reference",
    grantId: "local",
    scopeId: "vault",
  };
}

function startDaemon(binary, arguments_) {
  const child = spawn(binary, arguments_, {
    cwd: repository,
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  return { child, stderr: () => stderr };
}

function waitForExit(child, description, timeout = 5_000) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  }
  return new Promise((resolve_, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`${description} did not exit within ${timeout}ms`));
    }, timeout);
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

async function stopDaemon(daemon, signal) {
  if (daemon === undefined) {
    return undefined;
  }
  const { child } = daemon;
  if (child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
  }
  return waitForExit(child, "installed companion");
}

function callLauncher(launcher, endpoint, request) {
  return new Promise((resolve_, reject) => {
    const child = spawn(launcher, ["plugin-session", "--endpoint", endpoint], {
      cwd: repository,
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
    }, 5_000);
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
            `installed launcher exited with ${signal ?? `code ${code}`}: ${stderr}`,
          ),
        );
        return;
      }
      try {
        const messages = parseFramedJson(Buffer.concat(stdout));
        assert.equal(messages.length, 1, "launcher returns exactly one response");
        resolve_(response ?? messages[0]);
      } catch (error) {
        reject(error);
      }
    });
    try {
      child.stdin.write(framedJson(request));
    } catch (error) {
      fail(error);
    }
  });
}

async function waitForLauncherResponse(launcher, endpoint, request, predicate) {
  const deadline = Date.now() + 5_000;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const response = await callLauncher(launcher, endpoint, request());
      if (predicate(response)) {
        return response;
      }
      lastError = new Error(`unexpected launcher response: ${JSON.stringify(response)}`);
    } catch (error) {
      lastError = error;
    }
    await sleep(50);
  }
  throw lastError ?? new Error("installed launcher never received a response");
}

async function waitForAbsent(path, description) {
  const deadline = Date.now() + 5_000;
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
  throw new Error(`${description} remained after 5000ms`);
}

async function exerciseInstalledLifecycle(installRoot, workspace) {
  const vault = join(workspace, "installed-lifecycle-vault");
  const endpoint = join(workspace, "installed-runtime", "grimmore.sock");
  const database = join(workspace, "installed-lifecycle.sqlite3");
  const daemonBinary = join(installRoot, "bin", "grimmored");
  const launcherBinary = join(installRoot, "bin", "grimmore-launcher");
  const daemonArguments = [
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
    endpoint,
  ];
  let requestId = 0;
  let daemon;

  const waitForHealth = () =>
    waitForLauncherResponse(
      launcherBinary,
      endpoint,
      () => pluginRequest(++requestId, "system.health", {}),
      (response) => response.result?.status === "ok" && response.result?.role === "plugin",
    );

  try {
    await cp(referenceVault, vault, { recursive: true });

    daemon = startDaemon(daemonBinary, daemonArguments);
    await waitForHealth();
    const cleanStop = await stopDaemon(daemon, "SIGINT");
    assert.equal(cleanStop?.code, 0, `installed daemon stopped cleanly: ${daemon.stderr()}`);
    daemon = undefined;
    await waitForAbsent(endpoint, "cleanly stopped companion endpoint");

    daemon = startDaemon(daemonBinary, daemonArguments);
    await waitForHealth();
    const crash = await stopDaemon(daemon, "SIGKILL");
    assert.equal(crash?.signal, "SIGKILL", `installed daemon crashed as requested: ${daemon.stderr()}`);
    daemon = undefined;

    daemon = startDaemon(daemonBinary, daemonArguments);
    await waitForHealth();
    const watchedNote = join(vault, "daily", "2026-07-13.md");
    const original = await readFile(watchedNote, "utf8");
    await writeFile(
      watchedNote,
      `${original}\nInstalled lifecycle watcher recovery sentinel.\n`,
      "utf8",
    );
    await waitForLauncherResponse(
      launcherBinary,
      endpoint,
      () =>
        pluginRequest(++requestId, "knowledge.search", {
          query: "installed lifecycle watcher recovery sentinel",
          limit: 5,
        }),
      (response) =>
        response.result?.hits?.some((hit) => hit.path === "daily/2026-07-13.md") === true,
    );

    const finalStop = await stopDaemon(daemon, "SIGINT");
    assert.equal(finalStop?.code, 0, `restarted daemon stopped cleanly: ${daemon.stderr()}`);
    daemon = undefined;
    await waitForAbsent(endpoint, "restarted companion endpoint");
  } finally {
    await stopDaemon(daemon, "SIGKILL").catch(() => {});
  }
}

function shellQuote(value) {
  return `'${value.replaceAll("'", "'\"'\"'")}'`;
}

function doctorScript(report) {
  return [
    "#!/usr/bin/env sh",
    'if [ "$#" -eq 1 ] && [ "$1" = "doctor" ]; then',
    `  printf '%s\\n' ${shellQuote(report)}`,
    "  exit 0",
    "fi",
    "exit 64",
    "",
  ].join("\n");
}

async function makeArtifact(workspace, version, options = {}) {
  const artifactName = `grimmore-${version}-${target}.tar.gz`;
  const payloadRoot = `grimmore-${version}-${target}`;
  const payloadParent = join(workspace, "payloads");
  const payload = join(payloadParent, payloadRoot);
  const artifact = join(workspace, artifactName);
  await mkdir(payload, { recursive: true, mode: 0o700 });
  for (const binary of ["grimmored", "grimmore-launcher"]) {
    const source = join(repository, "target/release", binary);
    const destination = join(payload, binary);
    if (binary === "grimmored" && options.invalidDaemon === true) {
      await writeFile(destination, "this is not a Grimmore companion\n", {
        mode: 0o700,
      });
    } else if (binary === "grimmored" && options.doctorReport !== undefined) {
      await writeFile(destination, doctorScript(options.doctorReport), { mode: 0o700 });
    } else {
      await copyFile(source, destination);
    }
    await chmod(destination, 0o700);
  }
  await run("tar", ["-C", payloadParent, "-czf", artifact, payloadRoot]);
  return { artifact, artifactName };
}

async function signManifest(workspace, artifact, version) {
  const manifest = join(workspace, `${version}.manifest.json`);
  const signature = join(workspace, `${version}.manifest.sig`);
  const gpgHome = join(workspace, "gnupg");
  await run(process.execPath, [
    manifestBuilder,
    "--artifact",
    artifact,
    "--channel",
    "test",
    "--created-at",
    "2026-07-13T00:00:00Z",
    "--out",
    manifest,
    "--target",
    target,
    "--version",
    version,
  ]);
  await run("gpg", [
    "--homedir",
    gpgHome,
    "--batch",
    "--yes",
    "--pinentry-mode",
    "loopback",
    "--passphrase",
    "",
    "--local-user",
    signer,
    "--detach-sign",
    "--output",
    signature,
    manifest,
  ]);
  return { manifest, signature };
}

function installerArguments(artifact, manifest, signature, keyring, installRoot) {
  return [
    installer,
    "--archive",
    artifact,
    "--manifest",
    manifest,
    "--signature",
    signature,
    "--keyring",
    keyring,
    "--install-root",
    installRoot,
  ];
}

test(
  "a real signed Linux payload stages, switches, verifies, and rolls back without privileges",
  { skip: process.platform !== "linux" || target === undefined, timeout: 300_000 },
  async () => {
    const workspace = await mkdtemp(join(tmpdir(), "grimmore-release-slice-"));
    try {
      assert.notEqual(
        process.getuid(),
        0,
        "the release smoke must run without administrator privileges",
      );
      const gpgHome = join(workspace, "gnupg");
      const keyring = join(workspace, "test-signer.gpg");
      const installRoot = join(workspace, "installed");
      await mkdir(gpgHome, { mode: 0o700 });
      await run("cargo", [
        "build",
        "--release",
        "--locked",
        "-p",
        "grimmored",
        "-p",
        "grimmore-launcher",
      ]);
      await run("gpg", [
        "--homedir",
        gpgHome,
        "--batch",
        "--yes",
        "--pinentry-mode",
        "loopback",
        "--passphrase",
        "",
        "--quick-generate-key",
        signer,
        "ed25519",
        "sign",
        "1d",
      ]);
      await run("gpg", [
        "--homedir",
        gpgHome,
        "--batch",
        "--yes",
        "--output",
        keyring,
        "--export",
        signer,
      ]);

      const firstVersion = "0.1.0-slice-a";
      const firstArtifact = await makeArtifact(workspace, firstVersion);
      const firstSignature = await signManifest(workspace, firstArtifact.artifact, firstVersion);
      const invalidSignature = join(workspace, "invalid.manifest.sig");
      await writeFile(invalidSignature, "not a detached signature\n", { mode: 0o600 });
      await assert.rejects(
        run(
          "bash",
          installerArguments(
            firstArtifact.artifact,
            firstSignature.manifest,
            invalidSignature,
            keyring,
            join(workspace, "invalid-signature-install"),
          ),
        ),
        /manifest signature verification failed/u,
      );
      await run(
        "bash",
        installerArguments(
          firstArtifact.artifact,
          firstSignature.manifest,
          firstSignature.signature,
          keyring,
          installRoot,
        ),
      );
      assert.equal(await readlink(join(installRoot, "current")), `versions/${firstVersion}`);
      const firstDoctor = await run(join(installRoot, "bin/grimmored"), ["doctor"]);
      assert.equal(JSON.parse(firstDoctor.stdout).fts5Available, true);
      await exerciseInstalledLifecycle(installRoot, workspace);
      await run("node", [
        installedLifecycle,
        "--daemon",
        join(installRoot, "bin", "grimmored"),
        "--launcher",
        join(installRoot, "bin", "grimmore-launcher"),
        "--fixture-vault",
        referenceVault,
        "--workspace",
        join(workspace, "installed-lifecycle-mcp"),
        "--endpoint",
        join(workspace, "installed-runtime-mcp", "grimmore.sock"),
      ]);

      const secondVersion = "0.1.0-slice-b";
      const secondArtifact = await makeArtifact(workspace, secondVersion);
      const secondSignature = await signManifest(
        workspace,
        secondArtifact.artifact,
        secondVersion,
      );
      const interruptedVersion = join(installRoot, "versions", secondVersion);
      await mkdir(interruptedVersion, { recursive: true, mode: 0o700 });
      await writeFile(join(interruptedVersion, "partial"), "interrupted before commit\n", {
        mode: 0o600,
      });
      await run(
        "bash",
        installerArguments(
          secondArtifact.artifact,
          secondSignature.manifest,
          secondSignature.signature,
          keyring,
          installRoot,
        ),
      );
      assert.equal(await readlink(join(installRoot, "current")), `versions/${secondVersion}`);
      assert.equal(await readlink(join(installRoot, "previous")), `versions/${firstVersion}`);
      await assert.rejects(readFile(join(interruptedVersion, "partial")));

      await run("bash", [rollback, "--install-root", installRoot]);
      assert.equal(await readlink(join(installRoot, "current")), `versions/${firstVersion}`);
      assert.equal(await readlink(join(installRoot, "previous")), `versions/${secondVersion}`);
      await run(
        "bash",
        installerArguments(
          secondArtifact.artifact,
          secondSignature.manifest,
          secondSignature.signature,
          keyring,
          installRoot,
        ),
      );
      assert.equal(await readlink(join(installRoot, "current")), `versions/${secondVersion}`);

      const unavailableSecretService = join(workspace, "no-secret-service.sock");
      await assert.rejects(
        run("env", [
          `DBUS_SESSION_BUS_ADDRESS=unix:path=${unavailableSecretService}`,
          "bash",
          rollback,
          "--install-root",
          installRoot,
        ]),
        /rollback companion failed its health check/u,
      );
      assert.equal(await readlink(join(installRoot, "current")), `versions/${secondVersion}`);
      assert.equal(await readlink(join(installRoot, "previous")), `versions/${firstVersion}`);

      const rollbackHealthRoot = join(workspace, "rollback-health-validation");
      const rollbackCurrent = join(rollbackHealthRoot, "versions", secondVersion);
      const rollbackPrevious = join(rollbackHealthRoot, "versions", firstVersion);
      await mkdir(join(rollbackHealthRoot, "bin"), { recursive: true, mode: 0o700 });
      await mkdir(rollbackCurrent, { recursive: true, mode: 0o700 });
      await mkdir(rollbackPrevious, { recursive: true, mode: 0o700 });
      await writeFile(join(rollbackCurrent, ".ready"), "", { mode: 0o600 });
      await writeFile(join(rollbackPrevious, ".ready"), "", { mode: 0o600 });
      await writeFile(
        join(rollbackPrevious, "grimmored"),
        doctorScript(
          '{"fts5Available":true,"protocolVersion":true,"credentialStoreAvailable":true}',
        ),
        { mode: 0o700 },
      );
      await symlink(`versions/${secondVersion}`, join(rollbackHealthRoot, "current"));
      await symlink(`versions/${firstVersion}`, join(rollbackHealthRoot, "previous"));
      await assert.rejects(
        run("bash", [rollback, "--install-root", rollbackHealthRoot]),
        /rollback companion failed its health check/u,
      );
      assert.equal(
        await readlink(join(rollbackHealthRoot, "current")),
        `versions/${secondVersion}`,
      );
      assert.equal(
        await readlink(join(rollbackHealthRoot, "previous")),
        `versions/${firstVersion}`,
      );

      const tamperedArtifact = join(workspace, "tampered", firstArtifact.artifactName);
      await mkdir(resolve(tamperedArtifact, ".."), { recursive: true, mode: 0o700 });
      const original = await readFile(firstArtifact.artifact);
      const alteredArchive = Buffer.from(original);
      alteredArchive[Math.floor(alteredArchive.byteLength / 2)] ^= 0xff;
      await writeFile(tamperedArtifact, alteredArchive, {
        mode: 0o600,
      });
      await assert.rejects(
        run(
          "bash",
          installerArguments(
            tamperedArtifact,
            firstSignature.manifest,
            firstSignature.signature,
            keyring,
            join(workspace, "tampered-install"),
          ),
        ),
        /artifact hash does not match the signed manifest/u,
      );

      const failedVersion = "0.1.0-slice-failed";
      const failedArtifact = await makeArtifact(workspace, failedVersion, {
        invalidDaemon: true,
      });
      const failedSignature = await signManifest(workspace, failedArtifact.artifact, failedVersion);
      const failedInstallRoot = join(workspace, "failed-health-install");
      await assert.rejects(
        run(
          "bash",
          installerArguments(
            failedArtifact.artifact,
            failedSignature.manifest,
            failedSignature.signature,
            keyring,
            failedInstallRoot,
          ),
        ),
        /staged companion failed its health check/u,
      );
      await assert.rejects(readlink(join(failedInstallRoot, "current")));
      await assert.rejects(readFile(join(failedInstallRoot, "versions", failedVersion, ".ready")));

      for (const [suffix, doctorReport] of [
        [
          "credential-store-missing",
          '{"fts5Available":true,"protocolVersion":1}',
        ],
        [
          "credential-store-unavailable",
          '{"fts5Available":true,"protocolVersion":1,"credentialStoreAvailable":false}',
        ],
        [
          "boolean-protocol-version",
          '{"fts5Available":true,"protocolVersion":true,"credentialStoreAvailable":true}',
        ],
        [
          "decimal-protocol-version",
          '{"fts5Available":true,"protocolVersion":1.0,"credentialStoreAvailable":true}',
        ],
      ]) {
        const unhealthyVersion = `0.1.0-slice-${suffix}`;
        const unhealthyArtifact = await makeArtifact(workspace, unhealthyVersion, {
          doctorReport,
        });
        const unhealthySignature = await signManifest(
          workspace,
          unhealthyArtifact.artifact,
          unhealthyVersion,
        );
        const unhealthyInstallRoot = join(workspace, `${suffix}-install`);
        await assert.rejects(
          run(
            "bash",
            installerArguments(
              unhealthyArtifact.artifact,
              unhealthySignature.manifest,
              unhealthySignature.signature,
              keyring,
              unhealthyInstallRoot,
            ),
          ),
          /staged companion failed its health check/u,
        );
        await assert.rejects(readlink(join(unhealthyInstallRoot, "current")));
        await assert.rejects(
          readFile(join(unhealthyInstallRoot, "versions", unhealthyVersion, ".ready")),
        );
      }

      const thirdVersion = "0.1.0-slice-c";
      const thirdArtifact = await makeArtifact(workspace, thirdVersion);
      const thirdSignature = await signManifest(workspace, thirdArtifact.artifact, thirdVersion);
      const stableLauncher = join(installRoot, "bin", "grimmore-launcher");
      await unlink(stableLauncher);
      await writeFile(stableLauncher, "foreign launcher\n", { mode: 0o700 });
      await assert.rejects(
        run(
          "bash",
          installerArguments(
            thirdArtifact.artifact,
            thirdSignature.manifest,
            thirdSignature.signature,
            keyring,
            installRoot,
          ),
        ),
        /stable launcher path already belongs to another installation/u,
      );
      assert.equal(await readlink(join(installRoot, "current")), `versions/${secondVersion}`);
      await readFile(join(installRoot, "versions", thirdVersion, ".ready"));
      await unlink(stableLauncher);
      await symlink("../current/grimmore-launcher", stableLauncher);
      await run(
        "bash",
        installerArguments(
          thirdArtifact.artifact,
          thirdSignature.manifest,
          thirdSignature.signature,
          keyring,
          installRoot,
        ),
      );
      assert.equal(await readlink(join(installRoot, "current")), `versions/${thirdVersion}`);
      assert.equal(await readlink(join(installRoot, "previous")), `versions/${secondVersion}`);
      await run("bash", [rollback, "--install-root", installRoot]);
      assert.equal(await readlink(join(installRoot, "current")), `versions/${secondVersion}`);
      assert.equal(await readlink(join(installRoot, "previous")), `versions/${thirdVersion}`);

      const storedManifest = await readFile(
        join(installRoot, "versions", firstVersion, "release-manifest.json"),
        "utf8",
      );
      const storedHash = createHash("sha256").update(storedManifest).digest("hex");
      const signedHash = createHash("sha256")
        .update(await readFile(firstSignature.manifest, "utf8"))
        .digest("hex");
      assert.equal(storedHash, signedHash, "the exact signed manifest remains with the payload");
    } finally {
      await rm(workspace, { recursive: true, force: true });
    }
  },
);
