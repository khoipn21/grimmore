import { spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { compile } from "json-schema-to-typescript";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const packageDirectory = resolve(scriptDirectory, "..");
const workspaceDirectory = resolve(packageDirectory, "../..");

const result = spawnSync(
  "cargo",
  ["run", "--quiet", "--locked", "-p", "grimmored", "--", "protocol-schema"],
  {
    cwd: workspaceDirectory,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"],
  },
);

if (result.status !== 0) {
  throw new Error(`schema export failed with status ${String(result.status)}`);
}

const schema = JSON.parse(result.stdout);
const schemaPath = resolve(packageDirectory, "schema/grimmore-rpc.json");
const generatedPath = resolve(packageDirectory, "src/generated.ts");
const source = await compile(schema, "WireContract", {
  bannerComment:
    "/* Generated from the Rust wire contract. Run `pnpm protocol:generate`; do not edit. */",
  format: true,
  style: {
    semi: true,
    singleQuote: false,
    trailingComma: "all",
  },
  unreachableDefinitions: true,
});

await mkdir(dirname(schemaPath), { recursive: true });
await writeFile(schemaPath, `${JSON.stringify(schema, null, 2)}\n`, "utf8");
await writeFile(generatedPath, source, "utf8");

console.log("Generated Rust/TypeScript protocol contracts.");
