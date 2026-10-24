export type StreamStatus = "active" | "paused" | "cancelled" | "archived";

export interface DashStream {
  id: number;
  recipient: string;
  deposited: number;
  vested: number;
  ratePerSec: number;
  status: StreamStatus;
}

export type InvariantError =
  | "ZERO_DEPOSIT"
  | "NEGATIVE_VALUE"
  | "NON_FINITE"
  | "VESTED_EXCEEDS_DEPOSIT"
  | "UNSAFE_INTEGER"
  | "BATCH_TOO_LARGE"
  | "EMPTY_RECIPIENT";

export const MAX_STREAMS = 1_000;

export class InvariantViolation extends Error {
  constructor(readonly code: InvariantError, detail: string) {
    super(`${code}: ${detail}`);
    this.name = "InvariantViolation";
  }
}

/** Reject degenerate rows before they reach the UI; never wraps or clamps silently. */
export function assertStreamInvariants(s: DashStream): void {
  if (!s.recipient || s.recipient.trim() === "") {
    throw new InvariantViolation("EMPTY_RECIPIENT", `stream ${s.id}`);
  }
  for (const v of [s.deposited, s.vested, s.ratePerSec]) {
    if (!Number.isFinite(v)) throw new InvariantViolation("NON_FINITE", `stream ${s.id}`);
    if (v < 0) throw new InvariantViolation("NEGATIVE_VALUE", `stream ${s.id}`);
  }
  if (s.deposited > Number.MAX_SAFE_INTEGER || s.vested > Number.MAX_SAFE_INTEGER) {
    throw new InvariantViolation("UNSAFE_INTEGER", `stream ${s.id}`);
  }
  if (s.deposited === 0) throw new InvariantViolation("ZERO_DEPOSIT", `stream ${s.id}`);
  if (s.vested > s.deposited) throw new InvariantViolation("VESTED_EXCEEDS_DEPOSIT", `stream ${s.id}`);
}

/** Vested percentage in [0, 100]; 0 when nothing is deposited. */
export function vestedPct(vested: number, deposited: number): number {
  if (!(deposited > 0) || !Number.isFinite(vested) || vested <= 0) return 0;
  return Math.min(100, Math.round((vested / deposited) * 100));
}

export interface Summary {
  active: number;
  totalVested: number;
  totalDeposited: number;
}

/** Validates every row and aggregates in a single pass over the list. */
export function summarize(streams: readonly DashStream[]): Summary {
  if (streams.length > MAX_STREAMS) {
    throw new InvariantViolation("BATCH_TOO_LARGE", `${streams.length} > ${MAX_STREAMS}`);
  }
  let active = 0;
  let totalVested = 0;
  let totalDeposited = 0;
  for (const s of streams) {
    assertStreamInvariants(s);
    if (s.status === "active") active++;
    totalVested += s.vested;
    totalDeposited += s.deposited;
    if (totalDeposited > Number.MAX_SAFE_INTEGER) {
      throw new InvariantViolation("UNSAFE_INTEGER", "aggregate deposited");
    }
  }
  return { active, totalVested, totalDeposited };
}

/** Fault-recovery wrapper that catches invariant errors and reports safe fallback. */
export function safeSummarize(streams: readonly DashStream[]): {
  summary: Summary;
  error?: InvariantViolation;
} {
  try {
    return { summary: summarize(streams) };
  } catch (err) {
    if (err instanceof InvariantViolation) {
      return {
        summary: { active: 0, totalVested: 0, totalDeposited: 0 },
        error: err,
      };
    }
    throw err;
  }
}

/** Return status badge styling with archival resilience. */
export function getStatusBadgeClass(status: StreamStatus): string {
  switch (status) {
    case "active":
      return "bg-emerald-500/10 text-emerald-300";
    case "paused":
      return "bg-amber-500/10 text-amber-300";
    case "archived":
      return "bg-purple-500/10 text-purple-300";
    case "cancelled":
    default:
      return "bg-rose-500/10 text-rose-300";
  }
}

