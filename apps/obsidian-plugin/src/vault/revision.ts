import { createHash } from "node:crypto";

const CONTENT_REVISION_PATTERN = /^sha256:[0-9a-f]{64}$/u;

export function contentRevision(content: string): string {
  return `sha256:${createHash("sha256").update(content, "utf8").digest("hex")}`;
}

export function isContentRevision(value: unknown): value is string {
  return typeof value === "string" && CONTENT_REVISION_PATTERN.test(value);
}
