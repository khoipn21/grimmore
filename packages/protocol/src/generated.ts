/* Generated from the Rust wire contract. Run `pnpm protocol:generate`; do not edit. */

/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "ClientHandshakeMessage".
 */
export type ClientHandshakeMessage =
  | {
      clientNonce: string;
      clientVersion: string;
      protocolVersion: number;
      role: SessionRole;
      type: "hello";
    }
  | {
      clientProof: string;
      sessionId: string;
      type: "authenticate";
    };
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "SessionRole".
 */
export type SessionRole = "plugin" | "mcp-readonly";
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "JsonRpcResponse".
 */
export type JsonRpcResponse = JsonRpcSuccess | JsonRpcFailure;
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "ServerHandshakeMessage".
 */
export type ServerHandshakeMessage =
  | {
      protocolVersion: number;
      serverNonce: string;
      serverProof: string;
      serverVersion: string;
      sessionId: string;
      type: "challenge";
    }
  | {
      expiresAtUnixMs: number;
      role: SessionRole;
      sessionId: string;
      type: "ready";
    }
  | {
      message: string;
      type: "rejected";
      [k: string]: unknown;
    };

/**
 * Schema aggregation point used to generate the committed TypeScript contract.
 */
export interface WireContract {
  cancelRequestParams: CancelRequestParams;
  cancelRequestResult: CancelRequestResult;
  clientAuthenticate: ClientAuthenticate;
  clientHandshakeMessage: ClientHandshakeMessage;
  clientHello: ClientHello;
  failure: JsonRpcFailure;
  healthResult: HealthResult;
  patchProposal: PatchProposal;
  proposeNoteReplacementParams: ProposeNoteReplacementParams;
  request: JsonRpcRequest;
  response: JsonRpcResponse;
  searchNotesParams: SearchNotesParams;
  searchNotesResult: SearchNotesResult;
  serverChallenge: ServerChallenge;
  serverHandshakeMessage: ServerHandshakeMessage;
  sessionReady: SessionReady;
  success: JsonRpcSuccess;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "CancelRequestParams".
 */
export interface CancelRequestParams {
  requestId: number;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "CancelRequestResult".
 */
export interface CancelRequestResult {
  cancelled: boolean;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "ClientAuthenticate".
 */
export interface ClientAuthenticate {
  clientProof: string;
  sessionId: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "ClientHello".
 */
export interface ClientHello {
  clientNonce: string;
  clientVersion: string;
  protocolVersion: number;
  role: SessionRole;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "JsonRpcFailure".
 */
export interface JsonRpcFailure {
  error: JsonRpcErrorBody;
  id?: number | null;
  jsonrpc: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "JsonRpcErrorBody".
 */
export interface JsonRpcErrorBody {
  code: number;
  message: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "HealthResult".
 */
export interface HealthResult {
  productVersion: string;
  protocolVersion: number;
  role: SessionRole;
  status: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "PatchProposal".
 */
export interface PatchProposal {
  expectedRevision: string;
  path: string;
  proposedRevision: string;
  replacement: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "ProposeNoteReplacementParams".
 */
export interface ProposeNoteReplacementParams {
  expectedRevision: string;
  path: string;
  replacement: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "JsonRpcRequest".
 */
export interface JsonRpcRequest {
  deadlineUnixMs: number;
  grantId: string;
  id: number;
  jsonrpc: string;
  method: string;
  params: unknown;
  scopeId: string;
  vaultId: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "JsonRpcSuccess".
 */
export interface JsonRpcSuccess {
  id: number;
  jsonrpc: string;
  result: unknown;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "SearchNotesParams".
 */
export interface SearchNotesParams {
  limit: number;
  query: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "SearchNotesResult".
 */
export interface SearchNotesResult {
  hits: SearchHit[];
  indexedRevision: number;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "SearchHit".
 */
export interface SearchHit {
  path: string;
  revision: string;
  snippet: string;
  title: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "ServerChallenge".
 */
export interface ServerChallenge {
  protocolVersion: number;
  serverNonce: string;
  serverProof: string;
  serverVersion: string;
  sessionId: string;
}
/**
 * This interface was referenced by `WireContract`'s JSON-Schema
 * via the `definition` "SessionReady".
 */
export interface SessionReady {
  expiresAtUnixMs: number;
  role: SessionRole;
  sessionId: string;
}
