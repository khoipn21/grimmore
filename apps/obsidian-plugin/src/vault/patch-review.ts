import type { PatchProposal } from "@grimmore/protocol";

import { contentRevision, isContentRevision } from "./revision.js";

const MAX_REVIEW_BYTES = 512 * 1024;
const MAX_REVIEW_LINES = 10_000;
const REVIEW_CONTEXT_LINES = 3;

export class PatchReviewError extends Error {
  public constructor(message: string) {
    super(message);
    this.name = "PatchReviewError";
  }
}

export interface PreparedPatchReview {
  proposal: PatchProposal;
  unifiedDiff: string;
}

export function validatePatchProposal(
  value: unknown,
  activePath: string,
): PatchProposal {
  if (!isRecord(value)) {
    throw new PatchReviewError("The companion returned an invalid patch proposal.");
  }
  const keys = Object.keys(value);
  if (
    keys.length !== 4 ||
    !["expectedRevision", "path", "proposedRevision", "replacement"].every(
      (key) => keys.includes(key),
    )
  ) {
    throw new PatchReviewError("The companion returned an invalid patch proposal.");
  }
  const { expectedRevision, path, proposedRevision, replacement } = value;
  if (
    typeof path !== "string" ||
    typeof replacement !== "string" ||
    !isContentRevision(expectedRevision) ||
    !isContentRevision(proposedRevision)
  ) {
    throw new PatchReviewError("The companion returned an invalid patch proposal.");
  }
  assertSafeActiveMarkdownPath(path, activePath);
  if (contentRevision(replacement) !== proposedRevision) {
    throw new PatchReviewError("The proposed content does not match its revision.");
  }
  return { expectedRevision, path, proposedRevision, replacement };
}

export function preparePatchReview(
  value: unknown,
  activePath: string,
  currentContent: string,
): PreparedPatchReview {
  const proposal = validatePatchProposal(value, activePath);
  if (contentRevision(currentContent) !== proposal.expectedRevision) {
    throw new PatchReviewError(
      "The note changed before review. Request a fresh proposal.",
    );
  }
  if (currentContent === proposal.replacement) {
    throw new PatchReviewError("The proposal does not change the note.");
  }
  return {
    proposal,
    unifiedDiff: buildUnifiedPreview(
      proposal.path,
      currentContent,
      proposal.replacement,
    ),
  };
}

export function assertSafeActiveMarkdownPath(
  proposedPath: string,
  activePath: string,
): void {
  if (proposedPath !== activePath || !isSafeMarkdownPath(proposedPath)) {
    throw new PatchReviewError(
      "The proposal does not target the exact active Markdown note.",
    );
  }
}

export function isSafeMarkdownPath(path: string): boolean {
  if (
    path.length === 0 ||
    Buffer.byteLength(path, "utf8") > 1024 ||
    !path.endsWith(".md") ||
    path.includes("\\") ||
    path.startsWith("/") ||
    /^[A-Za-z]:/u.test(path) ||
    /[\p{Cc}\p{Cf}]/u.test(path)
  ) {
    return false;
  }
  const segments = path.split("/");
  if (
    segments.some(
      (segment) => segment.length === 0 || segment === "." || segment === "..",
    )
  ) {
    return false;
  }
  return !segments.some((segment) => segment.startsWith("."));
}

export function buildUnifiedPreview(
  path: string,
  before: string,
  after: string,
): string {
  assertReviewBounds(before, after);
  const beforeLines = before.split("\n");
  const afterLines = after.split("\n");
  let prefix = 0;
  while (
    prefix < beforeLines.length &&
    prefix < afterLines.length &&
    beforeLines[prefix] === afterLines[prefix]
  ) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < beforeLines.length - prefix &&
    suffix < afterLines.length - prefix &&
    beforeLines[beforeLines.length - 1 - suffix] ===
      afterLines[afterLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const contextStart = Math.max(0, prefix - REVIEW_CONTEXT_LINES);
  const beforeChangeEnd = beforeLines.length - suffix;
  const afterChangeEnd = afterLines.length - suffix;
  const beforeContextEnd = Math.min(
    beforeLines.length,
    beforeChangeEnd + REVIEW_CONTEXT_LINES,
  );
  const afterContextEnd = Math.min(
    afterLines.length,
    afterChangeEnd + REVIEW_CONTEXT_LINES,
  );
  const oldStart = String(contextStart + 1);
  const oldCount = String(beforeContextEnd - contextStart);
  const newStart = String(contextStart + 1);
  const newCount = String(afterContextEnd - contextStart);
  const output = [
    `--- a/${path}`,
    `+++ b/${path}`,
    `@@ -${oldStart},${oldCount} +${newStart},${newCount} @@`,
  ];
  output.push(
    ...beforeLines.slice(contextStart, prefix).map((line) => ` ${line}`),
    ...beforeLines.slice(prefix, beforeChangeEnd).map((line) => `-${line}`),
    ...afterLines.slice(prefix, afterChangeEnd).map((line) => `+${line}`),
    ...afterLines
      .slice(afterChangeEnd, afterContextEnd)
      .map((line) => ` ${line}`),
  );
  if (before.endsWith("\n") !== after.endsWith("\n")) {
    output.push(
      before.endsWith("\n")
        ? "\\ Before ended with a newline"
        : "\\ Before did not end with a newline",
      after.endsWith("\n")
        ? "\\ After ends with a newline"
        : "\\ After does not end with a newline",
    );
  }
  return output.join("\n");
}

function assertReviewBounds(before: string, after: string): void {
  const byteLength = Buffer.byteLength(before, "utf8") + Buffer.byteLength(after, "utf8");
  const lineCount = before.split("\n").length + after.split("\n").length;
  if (byteLength > MAX_REVIEW_BYTES || lineCount > MAX_REVIEW_LINES) {
    throw new PatchReviewError(
      "This patch is too large for a complete safe review in Obsidian.",
    );
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
