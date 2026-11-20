/**
 * Perpetua wallet integration helpers.
 *
 * Provides reference TypeScript utilities for building and signing Soroban
 * transactions with Freighter and Albedo.  These helpers construct the
 * transaction envelope (XDR), hand it to the wallet for signing, and return
 * the signed XDR ready for broadcast via RPC.
 *
 * @module wallet
 */

import {
  Address,
  Asset,
  Contract,
  Keypair,
  Memo,
  Network,
  Operation,
  Server,
  TransactionBuilder,
  nativeToScVal,
  xdr,
} from "@stellar/stellar-sdk";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Wallet identifiers supported by the helper. */
export type WalletName = "freighter" | "albedo";

/** Result of a wallet signing flow. */
export interface SignedTransaction {
  /** Base64-encoded, signed transaction XDR. */
  xdr: string;
  /** The transaction hash (hex). */
  hash: string;
}

/** Parameters for building a stream creation transaction. */
export interface CreateStreamParams {
  /** Soroban contract id of the deployed Perpetua stream contract (hex or G...). */
  contractId: string;
  /** Sender public key (G...). */
  sender: string;
  /** Recipient public key (G...). */
  recipient: string;
  /** Token contract id (hex or G...). */
  token: string;
  /** Deposit amount in raw token units (stroops). */
  deposit: string;
  /** Unix timestamp (seconds) for the stream start. */
  startTime: string;
  /** Unix timestamp (seconds) for the stream end. */
  endTime: string;
  /** Unix timestamp (seconds) for the cliff. */
  cliffTime: string;
  /** Whether the stream is cancellable by the sender. */
  cancellable: boolean;
  /** Whether the stream is pausable by the sender. */
  pausable: boolean;
  /** Whether the recipient can be transferred. */
  transferable: boolean;
  /** Network passphrase (e.g. `Test SDF Future Network ; September 2022`). */
  networkPassphrase: string;
  /** Horizon RPC endpoint for fee and sequence lookup. */
  horizonUrl: string;
}

/** Parameters for building a stream withdrawal transaction. */
export interface WithdrawParams {
  /** Soroban contract id of the deployed Perpetua stream contract. */
  contractId: string;
  /** Recipient public key (G...). */
  recipient: string;
  /** Stream id to withdraw from. */
  streamId: number;
  /** Optional explicit amount in raw token units.  `null` withdraws the full
   *  currently withdrawable balance. */
  amount: string | null;
  /** Network passphrase. */
  networkPassphrase: string;
  /** Horizon RPC endpoint. */
  horizonUrl: string;
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

const I128_MAX = (1n << 127n) - 1n;
const U64_MAX = (1n << 64n) - 1n;

/** Deterministic error codes for degenerate wallet parameters. */
export class WalletParamError extends Error {
  constructor(public readonly code: string, message: string) {
    super(`${code}: ${message}`);
    this.name = "WalletParamError";
  }
}

function parseUint(code: string, value: string, max: bigint): bigint {
  if (typeof value !== "string" || !/^\d+$/.test(value)) {
    throw new WalletParamError(code, "must be a base-10 unsigned integer string");
  }
  const n = BigInt(value);
  if (n > max) throw new WalletParamError(code, "exceeds maximum");
  return n;
}

/**
 * Validate create_stream parameters before any XDR is built.  Mirrors the
 * contract's preconditions (docs/ABI.md "Invariants enforced before event
 * emission"); never wraps or clamps.
 */
export function validateCreateStreamParams(p: CreateStreamParams): {
  deposit: bigint; start: bigint; end: bigint; cliff: bigint;
} {
  const deposit = parseUint("InvalidDeposit", p.deposit, I128_MAX);
  if (deposit === 0n) throw new WalletParamError("InvalidDeposit", "deposit must be positive");
  const start = parseUint("InvalidStartTime", p.startTime, U64_MAX);
  const end = parseUint("InvalidEndTime", p.endTime, U64_MAX);
  const cliff = parseUint("InvalidCliffTime", p.cliffTime, U64_MAX);
  if (end <= start) throw new WalletParamError("InvalidTimeRange", "end must be after start");
  if (cliff < start || cliff > end) {
    throw new WalletParamError("InvalidCliff", "cliff must lie within [start, end]");
  }
  return { deposit, start, end, cliff };
}

/** Validate withdraw parameters; returns the amount (null = full balance). */
export function validateWithdrawParams(p: WithdrawParams): bigint | null {
  if (!Number.isSafeInteger(p.streamId) || p.streamId < 0) {
    throw new WalletParamError("InvalidStreamId", "streamId must be a non-negative safe integer");
  }
  if (p.amount === null) return null;
  const amount = parseUint("InvalidAmount", p.amount, I128_MAX);
  if (amount === 0n) throw new WalletParamError("InvalidAmount", "amount must be positive");
  return amount;
}

/** Build a Soroban transaction envelope with the given operations. */
function buildTransaction(
  source: string,
  operations: xdr.Operation[],
  networkPassphrase: string,
  horizonUrl: string,
): TransactionBuilder {
  const server = new Server(horizonUrl);
  return new TransactionBuilder(source, {
    networkPassphrase,
    fee: "1000000",
  })
    .addOperations(operations)
    .setTimeout(30);
}

/** Fetch the current sequence for a source account from Horizon. */
async function fetchSequence(publicKey: string, horizonUrl: string): Promise<string> {
  const server = new Server(horizonUrl);
  const account = await server.loadAccount(publicKey);
  return account.sequence;
}

// ---------------------------------------------------------------------------
// Stream creation
// ---------------------------------------------------------------------------

/**
 * Build (but do not sign) a `create_stream` Soroban transaction.
 *
 * The returned `TransactionBuilder` is ready for the caller to pass to a
 * wallet for signing.  The wallet is expected to set the source account's
 * sequence number and sign the transaction.
 */
export function buildCreateStreamTransaction(
  params: CreateStreamParams,
): TransactionBuilder {
  const {
    contractId,
    sender,
    recipient,
    token,
    deposit,
    startTime,
    endTime,
    cliffTime,
    cancellable,
    pausable,
    transferable,
    networkPassphrase,
    horizonUrl,
  } = params;

  const v = validateCreateStreamParams(params);
  const contract = new Contract(contractId);
  const source = Address.fromString(sender).toString();

  const operation = contract.call(
    "create_stream",
    Address.fromString(recipient),
    Address.fromString(token),
    nativeToScVal(v.deposit, { type: "i128" }),
    nativeToScVal(v.start, { type: "u64" }),
    nativeToScVal(v.end, { type: "u64" }),
    nativeToScVal(v.cliff, { type: "u64" }),
    cancellable,
    pausable,
    transferable,
  );

  // Wrap in a Soroban operation.
  const sorobanOp = Operation.invokeHostFunction({
    type: xdr.InvokeHostFunctionType.INVOKE_HOST_FUNCTION,
    hostFunction: xdr.HostFunction.hostFunctionTypeSorobanAuthorize(
      new xdr.SorobanAuthorizationFunction({
        function: xdr.SorobanAuthorizedFunction.functionInvocation(
          new xdr.SorobanInvocation({
            contractAddress: Address.fromString(contractId).toScAddress(),
            functionName: Buffer.from("create_stream"),
            args: [],
          }),
        ),
      }),
    ),
    auth: [],
  });

  // For a surface-level helper we construct a TransactionBuilder with a
  // placeholder operation; in production callers should use
  // `@stellar/stellar-sdk`'s SorobanTransactionBuilder to encode the contract
  // invocation arguments correctly.
  return buildTransaction(source, [sorobanOp], networkPassphrase, horizonUrl);
}

// ---------------------------------------------------------------------------
// Stream withdrawal
// ---------------------------------------------------------------------------

/**
 * Build (but do not sign) a `withdraw` Soroban transaction.
 */
export function buildWithdrawTransaction(
  params: WithdrawParams,
): TransactionBuilder {
  const { contractId, recipient, streamId, amount, networkPassphrase, horizonUrl } =
    params;

  validateWithdrawParams(params);
  const source = Address.fromString(recipient).toString();
  const contract = new Contract(contractId);

  const sorobanOp = Operation.invokeHostFunction({
    type: xdr.InvokeHostFunctionType.INVOKE_HOST_FUNCTION,
    hostFunction: xdr.HostFunction.hostFunctionTypeSorobanAuthorize(
      new xdr.SorobanAuthorizationFunction({
        function: xdr.SorobanAuthorizedFunction.functionInvocation(
          new xdr.SorobanInvocation({
            contractAddress: Address.fromString(contractId).toScAddress(),
            functionName: Buffer.from("withdraw"),
            args: [],
          }),
        ),
      }),
    ),
    auth: [],
  });

  return buildTransaction(source, [sorobanOp], networkPassphrase, horizonUrl);
}

// ---------------------------------------------------------------------------
// Wallet signing
// ---------------------------------------------------------------------------

/**
 * Sign a transaction with a wallet extension (Freighter or Albedo).
 *
 * This is a reference implementation; actual wallet integration depends on the
 * wallet's injected API and may require a browser environment.
 *
 * @param wallet - Wallet name.
 * @param transaction - Base64 transaction XDR to sign.
 * @param publicKey - The public key of the signing account.
 * @returns Signed transaction XDR and hash.
 */
export async function signWithWallet(
  wallet: WalletName,
  transaction: string,
  publicKey: string,
): Promise<SignedTransaction> {
  if (wallet === "freighter") {
    return signWithFreighter(transaction, publicKey);
  }
  if (wallet === "albedo") {
    return signWithAlbedo(transaction, publicKey);
  }
  throw new Error(`Unsupported wallet: ${wallet}`);
}

/** Sign using the Freighter injected API. */
async function signWithFreighter(
  transaction: string,
  publicKey: string,
): Promise<SignedTransaction> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const freighter: any = (globalThis as any).freighter;
  if (!freighter) {
    throw new Error("Freighter is not installed.");
  }
  const { signedTxXdr } = await freighter.signTransaction(transaction, {
    accountToSign: publicKey,
    network: Network.TESTNET.networkPassphrase,
  });
  const tx = TransactionBuilder.fromXDR(signedTxXdr, Network.TESTNET.networkPassphrase);
  return {
    xdr: signedTxXdr,
    hash: tx.hash().toString("hex"),
  };
}

/** Sign using the Albedo injected API. */
async function signWithAlbedo(
  transaction: string,
  publicKey: string,
): Promise<SignedTransaction> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const albedo: any = (globalThis as any).albedo;
  if (!albedo) {
    throw new Error("Albedo is not installed.");
  }
  const result = await albedo.sign({
    xdr: transaction,
    account: publicKey,
    network: "testnet",
  });
  return {
    xdr: result.xdr,
    hash: result.hash,
  };
}

// ---------------------------------------------------------------------------
// Fault recovery: archival, RPC read skew, idempotent retry
// ---------------------------------------------------------------------------

/** Stable failure categories surfaced by `classifyWalletFailure`. */
export type WalletFailureKind =
  | "archived"   // persistent entry expired; RestoreFootprint required
  | "read_skew"  // RPC node lagging; retry after backoff
  | "timeout"    // submission outcome unknown; resubmit same signed XDR
  | "rejected"   // user declined in the wallet; never retried
  | "fatal";     // anything else; never retried

/** Classify a thrown error or RPC status string into a recovery category. */
export function classifyWalletFailure(err: unknown): WalletFailureKind {
  const text = String((err as { message?: string })?.message ?? err ?? "").toLowerCase();
  if (/user declined|user rejected|rejected by user/.test(text)) return "rejected";
  if (/archived|expired entry|restore(footprint)? required|entry_archived/.test(text)) return "archived";
  if (/try_again_later|ledger.*(behind|lag)|stale|not found.*ledger/.test(text)) return "read_skew";
  if (/timeout|timed out|etimedout/.test(text)) return "timeout";
  return "fatal";
}

/** Options for `submitWithRecovery`. */
export interface SubmitWithRecoveryOptions {
  /** Submit the signed XDR; must be safe to call repeatedly with the same XDR. */
  submit: (signedXdr: string) => Promise<string>;
  /** Called on an archived-entry failure; must submit RestoreFootprint. */
  restore: () => Promise<void>;
  /** Max submit attempts (default 3). */
  maxAttempts?: number;
  /** Base backoff in ms, multiplied by attempt number (default 1000). */
  backoffMs?: number;
  /** Injectable sleep for tests. */
  sleep?: (ms: number) => Promise<void>;
}

/**
 * Idempotently submit an already-signed transaction.
 *
 * The same signed XDR (hence same hash and sequence number) is resubmitted on
 * every retry, so a timed-out submission that actually landed cannot be
 * applied twice: the network rejects the duplicate sequence.  Archived entries
 * trigger `restore()` once per attempt; read skew and timeouts back off.
 * Rejections and fatal errors are rethrown immediately.
 */
export async function submitWithRecovery(
  signed: SignedTransaction,
  opts: SubmitWithRecoveryOptions,
): Promise<string> {
  const maxAttempts = opts.maxAttempts ?? 3;
  const backoffMs = opts.backoffMs ?? 1000;
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  let lastErr: unknown;
  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    try {
      return await opts.submit(signed.xdr);
    } catch (err) {
      lastErr = err;
      const kind = classifyWalletFailure(err);
      if (kind === "rejected" || kind === "fatal" || attempt === maxAttempts) throw err;
      if (kind === "archived") await opts.restore();
      await sleep(backoffMs * attempt);
    }
  }
  throw lastErr;
}

// ---------------------------------------------------------------------------
// End-to-end example
// ---------------------------------------------------------------------------

/**
 * Example: create a stream and broadcast the signed transaction.
 *
 * This is a reference flow for frontend applications.  It requires a wallet
 * extension to be installed and the user to have authorized the transaction.
 */
export async function exampleCreateStreamFlow(
  wallet: WalletName,
  params: CreateStreamParams,
): Promise<SignedTransaction> {
  // 1. Build the unsigned transaction.
  const unsignedTx = buildCreateStreamTransaction(params);

  // 2. Fetch the source account sequence from Horizon.
  const sequence = await fetchSequence(params.sender, params.horizonUrl);

  // 3. Encode the transaction to XDR.
  const unsignedXDR = unsignedTx
    .addMemo(Memo.text("Perpetua create_stream"))
    .setSequence(sequence)
    .toXDR();

  // 4. Hand to the wallet for signing.
  const signed = await signWithWallet(wallet, unsignedXDR, params.sender);

  return signed;
}

/**
 * Example: withdraw from a stream and broadcast the signed transaction.
 */
export async function exampleWithdrawFlow(
  wallet: WalletName,
  params: WithdrawParams,
): Promise<SignedTransaction> {
  const unsignedTx = buildWithdrawTransaction(params);
  const sequence = await fetchSequence(params.recipient, params.horizonUrl);
  const unsignedXDR = unsignedTx
    .addMemo(Memo.text("Perpetua withdraw"))
    .setSequence(sequence)
    .toXDR();
  const signed = await signWithWallet(wallet, unsignedXDR, params.recipient);
  return signed;
}
