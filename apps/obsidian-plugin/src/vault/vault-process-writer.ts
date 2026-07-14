import type { PatchProposal } from "@grimmore/protocol";
import type { TFile, Vault } from "obsidian";

import { validatePatchProposal } from "./patch-review.js";
import { contentRevision } from "./revision.js";

export class StaleRevisionError extends Error {
  public constructor() {
    super("The note changed after review. No content was written.");
    this.name = "StaleRevisionError";
  }
}

export class WrittenRevisionError extends Error {
  public constructor() {
    super("Obsidian did not return the reviewed content revision.");
    this.name = "WrittenRevisionError";
  }
}

export interface AppliedPatch {
  path: string;
  revision: string;
}

export async function applyRevisionCheckedPatch(
  vault: Vault,
  file: TFile,
  value: PatchProposal,
): Promise<AppliedPatch> {
  const proposal = validatePatchProposal(value, file.path);
  const writtenContent = await vault.process(file, (latestContent) => {
    if (contentRevision(latestContent) !== proposal.expectedRevision) {
      throw new StaleRevisionError();
    }
    return proposal.replacement;
  });
  const writtenRevision = contentRevision(writtenContent);
  if (writtenRevision !== proposal.proposedRevision) {
    throw new WrittenRevisionError();
  }
  return { path: proposal.path, revision: writtenRevision };
}
