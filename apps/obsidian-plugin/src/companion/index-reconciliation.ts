import type { PatchProposal, ProposeNoteReplacementParams } from "@grimmore/protocol";

import { CompanionRequestError } from "./plugin-session-client.js";

const INDEX_RECONCILIATION_ERROR_CODE = -32007;
const INDEX_RECONCILIATION_RETRY_DELAY_MS = 200;
const INDEX_RECONCILIATION_RETRY_ATTEMPTS = 5;

export interface PatchProposalClient {
  proposeNoteReplacement(
    params: ProposeNoteReplacementParams,
  ): Promise<PatchProposal>;
}

export async function proposeAfterIndexReconciliation(
  client: PatchProposalClient,
  params: ProposeNoteReplacementParams,
  isUnloaded: () => boolean,
  delay: () => Promise<void> = () =>
    new Promise<void>((resolve) => {
      setTimeout(resolve, INDEX_RECONCILIATION_RETRY_DELAY_MS);
    }),
): Promise<PatchProposal> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await client.proposeNoteReplacement(params);
    } catch (error) {
      const shouldRetry =
        error instanceof CompanionRequestError &&
        error.code === INDEX_RECONCILIATION_ERROR_CODE &&
        attempt + 1 < INDEX_RECONCILIATION_RETRY_ATTEMPTS;
      if (!shouldRetry || isUnloaded()) {
        throw error;
      }
      await delay();
      if (isUnloaded()) {
        throw error;
      }
    }
  }
}
