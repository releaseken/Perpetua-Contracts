import { describe, expect, it } from "vitest";
import { Account, Address, Keypair, Networks, xdr } from "@stellar/stellar-sdk";
import {
  buildRestoreFootprintTransaction,
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
