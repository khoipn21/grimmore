import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { stat, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";
import { finished } from "node:stream/promises";

const RELEASE_SCHEMA =
  "https://grimmore.dev/schemas/release-manifest-v1.json";
const TARGETS = new Set([
  "windows-x64",
  "windows-arm64",
  "macos-x64",
  "macos-arm64",
  "linux-x64-gnu",
  "linux-arm64-gnu",
]);
const VERSION_PATTERN = /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$/u;
const TIMESTAMP_PATTERN =
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{3})?Z$/u;

function usage() {
  return [
    "Usage: node release/create-slice-manifest.mjs \\",
    "  --artifact <archive> --channel <test|stable> --created-at <UTC timestamp> \\",
    "  --out <manifest.json> --target <target> --version <version> \\",
    "  [--protocol-min <1..65535>] [--protocol-max <1..65535>]",
  ].join("\n");
}

function parseArguments(arguments_) {
  const values = new Map();
  for (let index = 0; index < arguments_.length; index += 2) {
    const option = arguments_[index];
    const value = arguments_[index + 1];
    if (!option?.startsWith("--") || value === undefined || values.has(option)) {
      throw new Error(usage());
    }
    values.set(option, value);
  }
  const expected = new Set([
    "--artifact",
    "--channel",
    "--created-at",
    "--out",
    "--target",
    "--version",
    "--protocol-min",
    "--protocol-max",
  ]);
  for (const option of values.keys()) {
    if (!expected.has(option)) {
      throw new Error(usage());
    }
  }
  for (const required of [
    "--artifact",
    "--channel",
    "--created-at",
    "--out",
    "--target",
    "--version",
  ]) {
    if (!values.has(required)) {
      throw new Error(usage());
    }
  }
  return values;
}

function protocolNumber(value, option) {
  if (!/^[1-9][0-9]{0,4}$/u.test(value)) {
    throw new Error(`${option} must be an integer between 1 and 65535`);
  }
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number > 65535) {
    throw new Error(`${option} must be an integer between 1 and 65535`);
  }
  return number;
}

async function sha256(file) {
  const hash = createHash("sha256");
  const stream = createReadStream(file);
  stream.on("data", (chunk) => hash.update(chunk));
  await finished(stream);
  return hash.digest("hex");
}

async function main() {
  const values = parseArguments(process.argv.slice(2));
  const artifact = resolve(values.get("--artifact"));
  const output = resolve(values.get("--out"));
  const channel = values.get("--channel");
  const createdAt = values.get("--created-at");
  const target = values.get("--target");
  const version = values.get("--version");
  const protocolMinimum = protocolNumber(
    values.get("--protocol-min") ?? "1",
    "--protocol-min",
  );
  const protocolMaximum = protocolNumber(
    values.get("--protocol-max") ?? "1",
    "--protocol-max",
  );

  if (channel !== "test" && channel !== "stable") {
    throw new Error("--channel must be test or stable");
  }
  if (!TARGETS.has(target)) {
    throw new Error("--target is not in release/targets.toml");
  }
  if (!VERSION_PATTERN.test(version)) {
    throw new Error("--version must be a normalized semantic version");
  }
  if (!TIMESTAMP_PATTERN.test(createdAt) || Number.isNaN(Date.parse(createdAt))) {
    throw new Error("--created-at must be a UTC ISO-8601 timestamp");
  }
  if (protocolMinimum > protocolMaximum) {
    throw new Error("--protocol-min cannot exceed --protocol-max");
  }

  const artifactName = basename(artifact);
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*\.(?:tar\.gz|zip)$/u.test(artifactName)) {
    throw new Error("--artifact must be a .tar.gz or .zip file with a portable name");
  }
  const metadata = await stat(artifact);
  if (!metadata.isFile() || metadata.size < 1) {
    throw new Error("--artifact must be a non-empty regular file");
  }

  const manifest = {
    $schema: RELEASE_SCHEMA,
    schemaVersion: 1,
    channel,
    version,
    target,
    createdAt,
    artifact: {
      file: artifactName,
      sha256: await sha256(artifact),
      size: metadata.size,
    },
    protocol: {
      minimum: protocolMinimum,
      maximum: protocolMaximum,
    },
  };
  await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o600,
  });
}

main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
  process.exitCode = 1;
});
