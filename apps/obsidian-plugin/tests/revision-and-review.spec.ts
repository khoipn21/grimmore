import { describe, expect, it } from "vitest";

import {
  buildUnifiedPreview,
  isSafeMarkdownPath,
  preparePatchReview,
  validatePatchProposal,
} from "../src/vault/patch-review.js";
import { contentRevision, isContentRevision } from "../src/vault/revision.js";

describe("content revisions", () => {
  it("matches the algorithm-qualified Rust SHA-256 contract", () => {
    expect(contentRevision("hello")).toBe(
      "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
    );
    expect(isContentRevision(contentRevision("hello"))).toBe(true);
    expect(isContentRevision("2cf24dba")).toBe(false);
  });
});

describe("patch proposal safety", () => {
  it("accepts only the exact active portable Markdown path", () => {
    expect(isSafeMarkdownPath("knowledge/ai/context.md")).toBe(true);
    for (const path of [
      "../escape.md",
      "/absolute.md",
      "C:/device.md",
      "knowledge\\note.md",
      ".obsidian/plugins/note.md",
      "knowledge/.hidden.md",
      "knowledge//note.md",
      "knowledge/line\nbreak.md",
      "knowledge/note.txt",
    ]) {
      expect(isSafeMarkdownPath(path), path).toBe(false);
    }
  });

  it("prepares an authentic exact-path proposal", () => {
    const before = "# Note\n\nBefore.\n";
    const replacement = "# Note\n\nAfter.\n";
    const review = preparePatchReview(
      {
        path: "knowledge/note.md",
        expectedRevision: contentRevision(before),
        proposedRevision: contentRevision(replacement),
        replacement,
      },
      "knowledge/note.md",
      before,
    );
    expect(review.proposal.replacement).toBe(replacement);
    expect(review.unifiedDiff).toContain("+After.");
  });

  it("rejects a forged replacement revision", () => {
    expect(() =>
      validatePatchProposal(
        {
          path: "note.md",
          expectedRevision: contentRevision("before"),
          proposedRevision: contentRevision("different"),
          replacement: "after",
        },
        "note.md",
      ),
    ).toThrow("does not match");
  });

  it("rejects ambiguous extra proposal fields", () => {
    const replacement = "after";
    expect(() =>
      validatePatchProposal(
        {
          path: "note.md",
          expectedRevision: contentRevision("before"),
          proposedRevision: contentRevision(replacement),
          replacement,
          writeWithoutReview: true,
        },
        "note.md",
      ),
    ).toThrow("invalid patch proposal");
  });

  it("requires the reviewed source revision to remain current", () => {
    const replacement = "after\n";
    expect(() =>
      preparePatchReview(
        {
          path: "note.md",
          expectedRevision: contentRevision("older\n"),
          proposedRevision: contentRevision(replacement),
          replacement,
        },
        "note.md",
        "current\n",
      ),
    ).toThrow("changed before review");
  });
});

describe("complete bounded diff preview", () => {
  it("shows every changed line as text", () => {
    const preview = buildUnifiedPreview(
      "note.md",
      "same\nold one\nold two\ntail",
      "same\nnew one\nnew two\ntail",
    );
    expect(preview).toContain("--- a/note.md");
    expect(preview).toContain("-old one");
    expect(preview).toContain("-old two");
    expect(preview).toContain("+new one");
    expect(preview).toContain("+new two");
  });

  it("rejects content too large to review instead of truncating it", () => {
    expect(() =>
      buildUnifiedPreview("note.md", "a".repeat(300_000), "b".repeat(300_000)),
    ).toThrow("too large");
  });

  it("makes a trailing-newline-only change explicit", () => {
    const preview = buildUnifiedPreview("note.md", "line", "line\n");
    expect(preview).toContain("Before did not end with a newline");
    expect(preview).toContain("After ends with a newline");
  });
});
