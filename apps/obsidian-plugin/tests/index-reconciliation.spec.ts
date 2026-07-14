import type {
  JsonRpcFailure,
  PatchProposal,
  ProposeNoteReplacementParams,
} from "@grimmore/protocol";
import { describe, expect, it } from "vitest";

import { CompanionRequestError } from "../src/companion/plugin-session-client.js";
import { proposeAfterIndexReconciliation } from "../src/companion/index-reconciliation.js";

const params = {
  path: "note.md",
  expectedRevision: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  replacement: "replacement\n",
};

const proposal: PatchProposal = {
  ...params,
  proposedRevision: "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
};

function requestError(code: number): CompanionRequestError {
  const failure: JsonRpcFailure = {
    jsonrpc: "2.0",
    id: 1,
    error: { code, message: "request failed" },
  };
  return new CompanionRequestError(failure);
}

describe("companion index reconciliation", () => {
  it("retries a transient stale-index response with the exact same proposal", async () => {
    const received: ProposeNoteReplacementParams[] = [];
    let calls = 0;
    const result = await proposeAfterIndexReconciliation(
      {
        proposeNoteReplacement: (value) => {
          received.push(value);
          calls += 1;
          if (calls === 1) {
            return Promise.reject(requestError(-32007));
          }
          return Promise.resolve(proposal);
        },
      },
      params,
      () => false,
      () => Promise.resolve(),
    );

    expect(result).toEqual(proposal);
    expect(received).toEqual([params, params]);
  });

  it("does not retry a terminal companion failure", async () => {
    let calls = 0;
    await expect(
      proposeAfterIndexReconciliation(
        {
          proposeNoteReplacement: () => {
            calls += 1;
            return Promise.reject(requestError(-32602));
          },
        },
        params,
        () => false,
        () => Promise.resolve(),
      ),
    ).rejects.toThrow("request failed");
    expect(calls).toBe(1);
  });

  it("stops after the bounded number of stale-index attempts", async () => {
    let calls = 0;
    await expect(
      proposeAfterIndexReconciliation(
        {
          proposeNoteReplacement: () => {
            calls += 1;
            return Promise.reject(requestError(-32007));
          },
        },
        params,
        () => false,
        () => Promise.resolve(),
      ),
    ).rejects.toThrow("request failed");
    expect(calls).toBe(5);
  });

  it("stops before a retry when the plugin unloads during backoff", async () => {
    let unloaded = false;
    let calls = 0;
    await expect(
      proposeAfterIndexReconciliation(
        {
          proposeNoteReplacement: () => {
            calls += 1;
            return Promise.reject(requestError(-32007));
          },
        },
        params,
        () => unloaded,
        () => {
          unloaded = true;
          return Promise.resolve();
        },
      ),
    ).rejects.toThrow("request failed");
    expect(calls).toBe(1);
  });
});
