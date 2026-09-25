import {
  Address,
  Account,
  Horizon,
  Keypair,
  Operation,
  SorobanDataBuilder,
  Transaction,
  TransactionBuilder,
  nativeToScVal,
  xdr,
} from "@stellar/stellar-sdk";

export interface RestoreFootprintParams {
  contractId: string;
  streamId: bigint | number | string;
  source: string;
  networkPassphrase: string;
  horizonUrl: string;
  fee?: string;
  timeoutSeconds?: number;
}

export interface RestoreFootprintResult {
  xdr: string;
  streamKey: xdr.LedgerKey;
  transaction: Transaction;
}

export function buildRestoreFootprintTransaction(
  account: Account,
  key: xdr.LedgerKey,
  networkPassphrase: string,
  fee = "1000000",
  timeoutSeconds = 30,
): Transaction {
  const sorobanData = restoreFootprintData(key);
  return new TransactionBuilder(account, { fee, networkPassphrase })
    .setSorobanData(sorobanData)
    .addOperation(Operation.restoreFootprint({}))
    .setTimeout(timeoutSeconds)
    .build();
}

/** Build Soroban transaction data with the stream entry in the write footprint. */
export function restoreFootprintData(key: xdr.LedgerKey): xdr.SorobanTransactionData {
  return new SorobanDataBuilder()
    .setFootprint([], [key])
    .setResources(0, 0, 0)
    .build();
}

/** Build the persistent ledger key used by DataKey::Stream(streamId). */
export function streamLedgerKey(contractId: string, streamId: bigint | number | string): xdr.LedgerKey {
  const id = BigInt(streamId);
  if (id < 0n || id > 18_446_744_073_709_551_615n) {
    throw new Error("stream_id must be an unsigned 64-bit integer");
  }

  const key = xdr.ScVal.scvVec([
    xdr.ScVal.scvSymbol("Stream"),
    nativeToScVal(id, { type: "u64" }),
  ]);

  return xdr.LedgerKey.contractData(new xdr.LedgerKeyContractData({
    contract: Address.fromString(contractId).toScAddress(),
    key,
    durability: xdr.ContractDataDurability.persistent(),
  }));
}

/**
 * Build and sign the exact RestoreFootprint transaction for one stream entry.
 * The source secret is used only locally and is never sent to Horizon.
 */
export async function buildSignedRestoreFootprint(
  params: RestoreFootprintParams,
): Promise<RestoreFootprintResult> {
  const key = streamLedgerKey(params.contractId, params.streamId);
  const keypair = Keypair.fromSecret(params.source);
  const server = new Horizon.Server(params.horizonUrl);
  const account = await server.loadAccount(keypair.publicKey());
  const transaction = buildRestoreFootprintTransaction(
    account,
    key,
    params.networkPassphrase,
    params.fee,
    params.timeoutSeconds,
  );

  transaction.sign(keypair);
  return { xdr: transaction.toXDR(), streamKey: key, transaction };
}

function usage(): never {
  throw new Error(
    "Usage: restore_footprint.ts --contract <C...> --stream-id <id> " +
      "--source-secret <S...> --horizon <url> --network-passphrase <passphrase>",
  );
}

function option(args: Map<string, string>, name: string): string {
  const value = args.get(name);
  if (!value) {
    throw new Error(`Missing required option: ${name}`);
  }
  return value;
}

/** CLI entrypoint for operators; secrets are accepted as an argument for local use only. */
export async function main(argv = process.argv.slice(2)): Promise<void> {
  const args = new Map<string, string>();
  for (let index = 0; index < argv.length; index += 2) {
    const name = argv[index];
    const value = argv[index + 1];
    if (!name?.startsWith("--") || value === undefined) {
      usage();
    }
    args.set(name, value);
  }

  const result = await buildSignedRestoreFootprint({
    contractId: option(args, "--contract"),
    streamId: option(args, "--stream-id"),
    source: option(args, "--source-secret"),
    horizonUrl: option(args, "--horizon"),
    networkPassphrase: option(args, "--network-passphrase"),
  });
  process.stdout.write(`${result.xdr}\n`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((error: unknown) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
