import { describe, expect, it, vi } from "vitest";
import { Account, Address, Keypair, Networks, xdr } from "@stellar/stellar-sdk";
import {
  buildRestoreFootprintTransaction,
  detectArchivedStream,
  restoreAndRetry,
  restoreFootprintData,
  streamLedgerKey,
} from "./restore_footprint";

describe("RestoreFootprint stream footprint", () => {
  it("encodes DataKey::Stream(42) canonically", () => {
    const key = streamLedgerKey(
      "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
      42n,
    );

    expect(key.contractData().key().toXDR("base64")).toBe(
      "AAAAEAAAAAEAAAACAAAADwAAAAZTdHJlYW0AAAAAAAUAAAAAAAAAKg==",
    );
    expect(key.contractData().durability()).toBe(
      xdr.ContractDataDurability.persistent(),
    );
  });

  it("puts only the stream entry in the restore write footprint", () => {
    const key = streamLedgerKey(
      "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
      42,
    );
    const data = restoreFootprintData(key);
    const footprint = data.resources().footprint();

    expect(footprint.readOnly()).toHaveLength(0);
    expect(footprint.readWrite()).toHaveLength(1);
    expect(footprint.readWrite()[0].toXDR("base64")).toBe(
      key.toXDR("base64"),
    );
  });

  it("accepts the archived probe's canonical canary key", () => {
    const canaryKey = xdr.LedgerKey.contractData(new xdr.LedgerKeyContractData({
      contract: Address.fromString(
        "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
      ).toScAddress(),
      key: xdr.ScVal.fromXDR(
        "AAAAEAAAAAEAAAABAAAADwAAAAZDYW5hcnkAAA==",
        "base64",
      ),
      durability: xdr.ContractDataDurability.persistent(),
    }));
    const footprint = restoreFootprintData(canaryKey).resources().footprint();

    expect(footprint.readOnly()).toHaveLength(0);
    expect(footprint.readWrite()[0].contractData().key().toXDR("base64")).toBe(
      "AAAAEAAAAAEAAAABAAAADwAAAAZDYW5hcnkAAA==",
    );
  });

  it("builds and signs a RestoreFootprint transaction", () => {
    const keypair = Keypair.random();
    const account = new Account(keypair.publicKey(), "7");
    const key = streamLedgerKey(
      "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4",
      42,
    );
    const transaction = buildRestoreFootprintTransaction(
      account,
      key,
      Networks.TESTNET,
    );

    transaction.sign(keypair);
    expect(transaction.toXDR()).toContain("AAAA");
    expect(transaction.operations).toHaveLength(1);
  });
});

describe("detectArchivedStream", () => {
  const CONTRACT = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

  it("reports archived when the entry is missing but the contract exists", async () => {
    const readEntry = vi.fn().mockResolvedValue(null);
    const readContract = vi.fn().mockResolvedValue({ id: CONTRACT });

    const result = await detectArchivedStream({
      contractId: CONTRACT,
      streamId: 42n,
      readEntry,
      readContract,
    });

    expect(result).toEqual({ status: "archived", streamId: 42n });
    expect(readEntry).toHaveBeenCalledTimes(1);
    expect(readContract).toHaveBeenCalledTimes(1);
  });

  it("reports never-existed when both the entry and contract are missing", async () => {
    const readEntry = vi.fn().mockResolvedValue(null);
    const readContract = vi.fn().mockResolvedValue(null);

    const result = await detectArchivedStream({
      contractId: CONTRACT,
      streamId: 7n,
      readEntry,
      readContract,
    });

    expect(result).toEqual({ status: "never-existed", streamId: 7n });
  });

  it("reports live when the entry is present", async () => {
    const readEntry = vi.fn().mockResolvedValue({ balance: 100n });
    const readContract = vi.fn();

    const result = await detectArchivedStream({
      contractId: CONTRACT,
      streamId: 42n,
      readEntry,
      readContract,
    });

    expect(result).toEqual({ status: "live", streamId: 42n });
    expect(readContract).not.toHaveBeenCalled();
  });
});

describe("restoreAndRetry idempotency", () => {
  const CONTRACT = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

  it("restores and retries once when the entry is archived", async () => {
    const restore = vi.fn().mockResolvedValue(undefined);
    const invoke = vi
      .fn()
      .mockRejectedValueOnce(new Error("entry archived"))
      .mockResolvedValueOnce("ok");

    const result = await restoreAndRetry({
      contractId: CONTRACT,
      streamId: 42n,
      invoke,
      restore,
    });

    expect(result).toBe("ok");
    expect(restore).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("is a no-op when re-run after a successful restore", async () => {
    const restore = vi.fn().mockResolvedValue(undefined);
    const invoke = vi.fn().mockResolvedValue("ok");

    const first = await restoreAndRetry({
      contractId: CONTRACT,
      streamId: 42n,
      invoke,
      restore,
    });
    const second = await restoreAndRetry({
      contractId: CONTRACT,
      streamId: 42n,
      invoke,
      restore,
    });

    expect(first).toBe("ok");
    expect(second).toBe("ok");
    expect(restore).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("does not restore when the invocation succeeds on the first try", async () => {
    const restore = vi.fn().mockResolvedValue(undefined);
    const invoke = vi.fn().mockResolvedValue("ok");

    const result = await restoreAndRetry({
      contractId: CONTRACT,
      streamId: 42n,
      invoke,
      restore,
    });

    expect(result).toBe("ok");
    expect(restore).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledTimes(1);
  });
});

describe("restoreAndRetry ledger skew", () => {
  const CONTRACT = "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4";

  it("retries when latestLedger disagrees across reads", async () => {
    const ledgers = [100, 101, 101];
    const readLatestLedger = vi.fn(() => ledgers.shift() ?? 101);
    const restore = vi.fn().mockResolvedValue(undefined);
    const invoke = vi.fn().mockResolvedValue("ok");

    const result = await restoreAndRetry({
      contractId: CONTRACT,
      streamId: 42n,
      invoke,
      restore,
      readLatestLedger,
    });

    expect(result).toBe("ok");
    expect(readLatestLedger).toHaveBeenCalledTimes(3);
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("gives up deterministically when skew persists", async () => {
    const readLatestLedger = vi.fn(() => 100);
    const restore = vi.fn().mockResolvedValue(undefined);
    const invoke = vi.fn().mockResolvedValue("ok");

    await expect(
      restoreAndRetry({
        contractId: CONTRACT,
        streamId: 42n,
        invoke,
        restore,
        readLatestLedger,
        maxSkewRetries: 2,
      }),
    ).rejects.toThrow(/ledger skew/i);
    expect(invoke).not.toHaveBeenCalled();
  });
});

import { describe as d2, expect as e2, it as i2 } from "vitest";
import { MAX_RESTORE_ATTEMPTS, STREAM_KEY_SCHEMA, restoreAndRetry as rr, streamLedgerKey as slk } from "./restore_footprint";

d2("restore_footprint hardening", () => {
  const C = "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC";
  i2("rejects out-of-range attempt/backoff caps", async () => {
    const read = async () => ({ streamExists: true, streamCount: 1, value: 1 });
    const base = { contractId: C, streamId: 0, source: "", networkPassphrase: "", horizonUrl: "" };
    await e2(rr({ ...base, maxAttempts: MAX_RESTORE_ATTEMPTS + 1 }, read)).rejects.toThrow(/maxAttempts/);
    await e2(rr({ ...base, backoffMs: -1 }, read)).rejects.toThrow(/backoffMs/);
  });
  i2("pins key schema and u64 bounds", () => {
    e2(STREAM_KEY_SCHEMA.variant).toBe("Stream");
    e2(() => slk(C, -1)).toThrow();
    e2(() => slk(C, 18_446_744_073_709_551_616n)).toThrow();
  });
});
