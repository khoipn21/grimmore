import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath, URL } from "node:url";

import { describe, expect, it } from "vitest";

const packageDirectory = resolve(fileURLToPath(new URL("..", import.meta.url)));

describe("generated protocol contract", () => {
  it("keeps strict Rust and TypeScript artifacts in sync", async () => {
    const [schemaSource, generatedSource] = await Promise.all([
      readFile(resolve(packageDirectory, "schema/grimmore-rpc.json"), "utf8"),
      readFile(resolve(packageDirectory, "src/generated.ts"), "utf8"),
    ]);
    const schema = JSON.parse(schemaSource);

    expect(schema.title).toBe("WireContract");
    expect(schema.additionalProperties).toBe(false);
    expect(schema.$defs.SessionRole.enum).toEqual(["plugin", "mcp-readonly"]);
    expect(generatedSource).toContain("export interface WireContract");
    expect(generatedSource).toContain('"plugin" | "mcp-readonly"');
  });
});
