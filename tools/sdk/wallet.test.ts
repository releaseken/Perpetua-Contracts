import { describe, expect, it, vi } from "vitest";
import { classifyWalletFailure, signWithWallet, submitWithRecovery } from "./wallet";

const signed = { xdr: "AAAA", hash: "ab" };
const noSleep = async () => {};

describe("classifyWalletFailure", () => {
  it.each([
    ["User declined access", "rejected"],
    ["entry_archived: restore required", "archived"],
    ["TRY_AGAIN_LATER", "read_skew"],
    ["request timed out", "timeout"],
    ["boom", "fatal"],
  ])("%s -> %s", (msg, kind) => {
    expect(classifyWalletFailure(new Error(msg))).toBe(kind);
  });
});

describe("submitWithRecovery", () => {
  it("restores then resubmits the same signed XDR after archival", async () => {
    const submit = vi.fn()
      .mockRejectedValueOnce(new Error("entry archived"))
      .mockResolvedValueOnce("hash1");
    const restore = vi.fn().mockResolvedValue(undefined);
    await expect(submitWithRecovery(signed, { submit, restore, sleep: noSleep })).resolves.toBe("hash1");
    expect(restore).toHaveBeenCalledTimes(1);
    expect(submit.mock.calls.map((c) => c[0])).toEqual(["AAAA", "AAAA"]);
  });

  it("retries read skew and timeouts without restoring", async () => {
    const submit = vi.fn()
      .mockRejectedValueOnce(new Error("TRY_AGAIN_LATER"))
      .mockRejectedValueOnce(new Error("timeout"))
      .mockResolvedValueOnce("h");
    const restore = vi.fn();
    await submitWithRecovery(signed, { submit, restore, sleep: noSleep });
    expect(restore).not.toHaveBeenCalled();
  });

  it("never retries rejections or fatal errors", async () => {
    const submit = vi.fn().mockRejectedValue(new Error("User declined"));
    await expect(submitWithRecovery(signed, { submit, restore: vi.fn(), sleep: noSleep })).rejects.toThrow("declined");
    expect(submit).toHaveBeenCalledTimes(1);
  });

  it("gives up after maxAttempts", async () => {
    const submit = vi.fn().mockRejectedValue(new Error("TRY_AGAIN_LATER"));
    await expect(submitWithRecovery(signed, { submit, restore: vi.fn(), maxAttempts: 2, sleep: noSleep })).rejects.toThrow();
    expect(submit).toHaveBeenCalledTimes(2);
  });
});

describe("signWithWallet errors", () => {
  it("rejects unsupported wallets and missing extensions", async () => {
    await expect(signWithWallet("nope" as never, "x", "G")).rejects.toThrow("Unsupported wallet");
    await expect(signWithWallet("freighter", "x", "G")).rejects.toThrow("not installed");
    await expect(signWithWallet("albedo", "x", "G")).rejects.toThrow("not installed");
  });
});

import { validateCreateStreamParams, validateWithdrawParams } from "./wallet";

describe("wallet invariants", () => {
  const ok = {
    contractId: "", sender: "", recipient: "", token: "", deposit: "100", startTime: "10",
    endTime: "20", cliffTime: "10", cancellable: true, pausable: true, transferable: true,
    networkPassphrase: "", horizonUrl: "",
  };
  it("accepts i128::MAX deposit and zero-duration cliff", () => {
    const r = validateCreateStreamParams({ ...ok, deposit: ((1n << 127n) - 1n).toString() });
    expect(r.cliff).toBe(10n);
  });
  it.each([
    [{ deposit: "0" }, "InvalidDeposit"],
    [{ deposit: (1n << 127n).toString() }, "InvalidDeposit"],
    [{ deposit: "-1" }, "InvalidDeposit"],
    [{ deposit: "1e3" }, "InvalidDeposit"],
    [{ endTime: "10" }, "InvalidTimeRange"],
    [{ cliffTime: "9" }, "InvalidCliff"],
    [{ cliffTime: "21" }, "InvalidCliff"],
    [{ startTime: (1n << 64n).toString() }, "InvalidStartTime"],
  ])("rejects %j", (patch, code) => {
    expect(() => validateCreateStreamParams({ ...ok, ...patch })).toThrow(code);
  });
  it("validates withdraw", () => {
    const w = { contractId: "", recipient: "", streamId: 1, amount: null, networkPassphrase: "", horizonUrl: "" };
    expect(validateWithdrawParams(w)).toBeNull();
    expect(() => validateWithdrawParams({ ...w, amount: "0" })).toThrow("InvalidAmount");
    expect(() => validateWithdrawParams({ ...w, streamId: -1 })).toThrow("InvalidStreamId");
  });
});
