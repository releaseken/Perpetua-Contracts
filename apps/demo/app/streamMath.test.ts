import { describe, expect, it } from "vitest";
import {
  getStatusBadgeClass,
  InvariantViolation,
  MAX_STREAMS,
  safeSummarize,
  summarize,
  vestedPct,
  type DashStream,
} from "./streamMath";
import { fmt, sampleStreams } from "./page";

const base: DashStream = {
  id: 1,
  recipient: "GALAX…7QKD",
  deposited: 100,
  vested: 50,
  ratePerSec: 1,
  status: "active",
};

const code = (fn: () => unknown) => {
  try {
    fn();
  } catch (e) {
    return (e as InvariantViolation).code;
  }
  return null;
};

describe("streamMath and dashboard unit tests", () => {
  it("covers happy paths across various stream statuses", () => {
    const mixed: DashStream[] = [
      base,
      { ...base, id: 2, status: "paused", deposited: 200, vested: 100 },
      { ...base, id: 3, status: "cancelled", deposited: 300, vested: 300 },
      { ...base, id: 4, status: "archived", deposited: 400, vested: 400 },
    ];
    const res = summarize(mixed);
    expect(res).toEqual({
      active: 1,
      totalVested: 850,
      totalDeposited: 1000,
    });
  });

  it("handles boundary cliffs", () => {
    // Minimum valid non-zero deposit
    expect(summarize([{ ...base, deposited: 1, vested: 0 }])).toEqual({
      active: 1,
      totalVested: 0,
      totalDeposited: 1,
    });
    // Exactly 100% vested cliff
    expect(summarize([{ ...base, deposited: 500, vested: 500 }])).toEqual({
      active: 1,
      totalVested: 500,
      totalDeposited: 500,
    });
    // Empty stream list
    expect(summarize([])).toEqual({ active: 0, totalVested: 0, totalDeposited: 0 });
    // Exactly at MAX_STREAMS limit
    const fullBatch = new Array(MAX_STREAMS).fill(base);
    expect(summarize(fullBatch).active).toBe(MAX_STREAMS);
  });

  it("rejects every error code deterministically", () => {
    // ZERO_DEPOSIT
    expect(code(() => summarize([{ ...base, deposited: 0, vested: 0 }]))).toBe("ZERO_DEPOSIT");
    // NEGATIVE_VALUE
    expect(code(() => summarize([{ ...base, deposited: -10 }]))).toBe("NEGATIVE_VALUE");
    expect(code(() => summarize([{ ...base, vested: -1 }]))).toBe("NEGATIVE_VALUE");
    expect(code(() => summarize([{ ...base, ratePerSec: -0.5 }]))).toBe("NEGATIVE_VALUE");
    // NON_FINITE
    expect(code(() => summarize([{ ...base, deposited: NaN }]))).toBe("NON_FINITE");
    expect(code(() => summarize([{ ...base, vested: Infinity }]))).toBe("NON_FINITE");
    expect(code(() => summarize([{ ...base, ratePerSec: -Infinity }]))).toBe("NON_FINITE");
    // VESTED_EXCEEDS_DEPOSIT
    expect(code(() => summarize([{ ...base, deposited: 100, vested: 101 }]))).toBe("VESTED_EXCEEDS_DEPOSIT");
    // UNSAFE_INTEGER
    expect(code(() => summarize([{ ...base, deposited: Number.MAX_SAFE_INTEGER + 1 }]))).toBe("UNSAFE_INTEGER");
    expect(code(() => summarize([{ ...base, vested: Number.MAX_SAFE_INTEGER + 1 }]))).toBe("UNSAFE_INTEGER");
    const big = Number.MAX_SAFE_INTEGER - 1;
    expect(
      code(() =>
        summarize([
          { ...base, deposited: big, vested: 0 },
          { ...base, id: 2, deposited: 10, vested: 0 },
        ])
      )
    ).toBe("UNSAFE_INTEGER");
    // BATCH_TOO_LARGE
    expect(code(() => summarize(new Array(MAX_STREAMS + 1).fill(base)))).toBe("BATCH_TOO_LARGE");
    // EMPTY_RECIPIENT
    expect(code(() => summarize([{ ...base, recipient: "" }]))).toBe("EMPTY_RECIPIENT");
    expect(code(() => summarize([{ ...base, recipient: "   " }]))).toBe("EMPTY_RECIPIENT");
  });

  it("provides fault-tolerant recovery via safeSummarize", () => {
    const invalidBatch = [{ ...base, deposited: 0 }];
    const res = safeSummarize(invalidBatch);
    expect(res.error).toBeDefined();
    expect(res.error?.code).toBe("ZERO_DEPOSIT");
    expect(res.summary).toEqual({ active: 0, totalVested: 0, totalDeposited: 0 });

    const validBatch = [base];
    const ok = safeSummarize(validBatch);
    expect(ok.error).toBeUndefined();
    expect(ok.summary.active).toBe(1);
  });

  it("maps status badge styling correctly", () => {
    expect(getStatusBadgeClass("active")).toContain("emerald");
    expect(getStatusBadgeClass("paused")).toContain("amber");
    expect(getStatusBadgeClass("archived")).toContain("purple");
    expect(getStatusBadgeClass("cancelled")).toContain("rose");
  });

  it("formats currency accurately with boundary handling", () => {
    expect(fmt(12345.67)).toBe("12,345.67");
    expect(fmt(0)).toBe("0");
    expect(fmt(NaN)).toBe("0.00");
    expect(fmt(Infinity)).toBe("0.00");
  });

  it("calculates vested percentage with boundary clamping", () => {
    expect(vestedPct(0, 100)).toBe(0);
    expect(vestedPct(50, 100)).toBe(50);
    expect(vestedPct(100, 100)).toBe(100);
    expect(vestedPct(200, 100)).toBe(100);
    expect(vestedPct(-10, 100)).toBe(0);
    expect(vestedPct(10, 0)).toBe(0);
    expect(vestedPct(NaN, 100)).toBe(0);
  });

  it("validates sampleStreams layout", () => {
    expect(sampleStreams.length).toBeGreaterThan(0);
    const { active, totalDeposited, totalVested } = summarize(sampleStreams);
    expect(active).toBe(2);
    expect(totalDeposited).toBeGreaterThan(0);
    expect(totalVested).toBeGreaterThan(0);
  });
});

