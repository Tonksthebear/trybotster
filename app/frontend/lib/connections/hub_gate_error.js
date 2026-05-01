export const HUB_GATE_ERROR_CODES = Object.freeze({
  UNAVAILABLE: "unavailable",
  TIMEOUT: "timeout",
  ABORTED: "aborted",
  DESTROYED: "destroyed",
  MISSING_TRANSPORT: "missing_transport",
  SEND_REJECTED: "send_rejected",
});

export class HubGateError extends Error {
  constructor(code, message, options = {}) {
    super(message || code);
    this.name = "HubGateError";
    this.code = code;
    if (options.cause) this.cause = options.cause;
  }
}

export function hubGateError(code, message, options = {}) {
  return new HubGateError(code, message, options);
}
