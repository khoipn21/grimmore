import { builtinModules } from "node:module";

import * as esbuild from "esbuild";

const production = process.argv[2] === "production";
const nodeBuiltins = builtinModules.flatMap((name) => [name, `node:${name}`]);

await esbuild.build({
  entryPoints: ["src/main.ts"],
  bundle: true,
  external: ["obsidian", "electron", ...nodeBuiltins],
  format: "cjs",
  logLevel: "info",
  minify: production,
  outfile: "main.js",
  platform: "node",
  sourcemap: production ? false : "inline",
  target: "es2022",
  treeShaking: true,
});
