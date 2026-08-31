/**
 * The SDK's error type.
 *
 * Lives in its own module because both the client and the launcher raise it,
 * and importing it from the client would make `launch` and `client` circular.
 */

/** A failure with a stable `code`, so callers branch on cause not message text. */
export class HarnessError extends Error {
  readonly code: string;
  readonly details?: unknown;
  constructor(code: string, message: string, details?: unknown) {
    super(`${code}: ${message}`);
    this.code = code;
    this.details = details;
    this.name = "HarnessError";
  }
}
