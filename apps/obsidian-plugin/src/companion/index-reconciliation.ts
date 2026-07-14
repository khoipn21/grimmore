import type { PatchProposal, ProposeNoteReplacementParams } from "@grimmore/protocol";

const INDEX_RECONCILIATION_ERROR_CODE = -32007;
const INDEX_RECONCILIATION_RETRY_DELAY_MS = 200;
const INDEX_RECONCILIATION_RETRY_ATTEMPTS = 5;

export interface PatchProposalClient {
  proposeNoteReplacement(
    params: ProposeNoteReplacementParams,
  ): Promise<PatchProposal>;
}

function isStaleIndexError(
  error: unknown,
): error is Error & { code: number } {
  if (!(error instanceof Error) || error.name !== "CompanionRequestError") {
    return false;
  }
  const code = (error as { code?: unknown }).code;
  return typeof code === "number" && code === INDEX_RECONCILIATION_ERROR_CODE;
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
        isStaleIndexError(error) &&
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
