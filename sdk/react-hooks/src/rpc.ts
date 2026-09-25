/**
 * Thin read-only client over the deployed Perpetua stream contract.
 *
 * This is the reference integration layer the hooks sit on top of. It talks to
 * a Soroban RPC endpoint via `@stellar/stellar-sdk` and decodes the contract's
 * ABI-shaped `Stream` struct (see `contracts/stream/abi/fluxora_stream.json`)
 * into the typed [`Stream`] used by [`useStream`] and [`useAccruedBalance`].
 *
 * Nothing here mutates state — it is the read side only. For authenticated
 * writes (`withdraw`, `cancel`, `pause`, ...) consumers should build operations
 * against the same `Contract` handle; see the package README for an example.
 */

import {
  Contract,
  SorobanRpc,
  StrKey,
  nativeToScVal,
  scValToNative,
  xdr,
} from '@stellar/stellar-sdk';

import { Stream, StreamStatus } from './types.js';

export interface BatchStreamLookupOptions {
  readonly contractId: string;
  readonly streamIds: bigint[];
}

export interface BatchStreamLookupHealth {
  readonly status?: string;
}

function streamIdFromKey(scVal: unknown): bigint | null {
  const native = Array.isArray(scVal)
    ? scVal
    : (scVal && typeof scVal === 'object' && 'switch' in (scVal as object)
        ? scValToNative(scVal as xdr.ScVal)
        : null);

  if (!Array.isArray(native) || native.length < 2 || native[0] !== 'Stream') {
    return null;
  }
  return toBigInt(native[1]);
}

function buildStreamLedgerKey(contractId: string, streamId: bigint): xdr.LedgerKey {
  const contractBytes = StrKey.decodeContract(contractId);
  return xdr.LedgerKey.contractData(
    new xdr.LedgerKeyContractData({
      contract: xdr.ScAddress.scAddressTypeContract(contractBytes),
      key: nativeToScVal(['Stream', streamId]),
      durability: xdr.ContractDataDurability.persistent(),
    }),
  );
}

export async function batchGetStreams(
  server: Pick<SorobanRpc.Server, 'getHealth' | 'getLedgerEntries'>,
  opts: BatchStreamLookupOptions,
): Promise<Map<bigint, Stream>> {
  const { contractId, streamIds } = opts;
  if (!streamIds.length) {
    return new Map();
  }

  const health = await server.getHealth();
  if (health?.status && health.status !== 'healthy' && health.status !== 'start-up') {
    throw new Error(`Soroban RPC is not healthy: ${health.status}`);
  }

  const uniqueIds = [...new Set(streamIds.map((id) => BigInt(id)))];
  const keys = uniqueIds.map((streamId) => buildStreamLedgerKey(contractId, streamId));
  const result = await server.getLedgerEntries(keys as any);
  const values = new Map<bigint, Stream>();

  for (const entry of result.entries ?? []) {
    const data = entry?.val?.contractData?.();
    if (!data) continue;

    const streamId = streamIdFromKey(data.key());
    if (streamId === null) continue;

    const stream = asStream(data.val(), streamId);
    values.set(streamId, stream);
  }

  return values;
}

/** Read-only operations the hooks need. Swap for a mock in tests. */
export interface PerpetuaClient {
  readonly rpcUrl: string;
  readonly contractId: string;
  /** Decode the full stream record for `id`. */
  getStream(id: bigint): Promise<Stream>;
  /** Total earned by the recipient since `start_time`, withdrawn or not. */
  vestedOf(id: bigint): Promise<bigint>;
  /** Amount the recipient could withdraw right now. */
  withdrawableOf(id: bigint): Promise<bigint>;
  /** Amount the sender would get back on an immediate cancel. */
  refundableOf(id: bigint): Promise<bigint>;
  /** Number of streams ever created. */
  streamCount(): Promise<bigint>;
  /** Current ledger close time in uint64 Unix seconds. */
  ledgerTimestamp(): Promise<bigint>;
}

export interface PerpetuaClientOptions {
  /** e.g. `https://soroban-testnet.stellar.org` */
  rpcUrl: string;
  /** The deployed `fluxora_stream` contract address. */
  contractId: string;
}

function addressFromScAddress(sc: xdr.ScAddress): string {
  if (sc.switch() === xdr.ScAddressType.scAddressTypeAccount()) {
    const account = sc.accountId();
    return StrKey.encodeEd25519PublicKey(account.ed25519());
  }
  return StrKey.encodeContract(sc.contractId());
}

function scalar(val: xdr.ScVal): bigint | number | boolean | string | null {
  return scValToNative(val) as bigint | number | boolean | string | null;
}

function toBigInt(value: bigint | number | boolean | string | null): bigint {
  if (typeof value === 'bigint') return value;
  if (value === null || value === undefined) return 0n;
  return BigInt(String(value));
}

function asStream(val: unknown, id: bigint): Stream {
  if (val && typeof val === 'object' && !('map' in (val as object)) && !('switch' in (val as object))) {
    const fields = val as Record<string, unknown>;
    const statusValue = Number(fields.status ?? 0);
    return {
      id,
      sender: String(fields.sender ?? ''),
      recipient: String(fields.recipient ?? ''),
      token: String(fields.token ?? ''),
      deposited: toBigInt(fields.deposited as bigint | number | boolean | string | null),
      withdrawn: toBigInt(fields.withdrawn as bigint | number | boolean | string | null),
      start_time: toBigInt(fields.start_time as bigint | number | boolean | string | null),
      end_time: toBigInt(fields.end_time as bigint | number | boolean | string | null),
      cliff_time: toBigInt(fields.cliff_time as bigint | number | boolean | string | null),
      cancellable: Boolean(fields.cancellable),
      pausable: Boolean(fields.pausable),
      transferable: Boolean(fields.transferable),
      paused_at: fields.paused_at == null ? null : toBigInt(fields.paused_at as bigint | number | boolean | string | null),
      paused_total: toBigInt(fields.paused_total as bigint | number | boolean | string | null),
      status: [0, 1, 2, 3].includes(statusValue) ? (statusValue as StreamStatus) : StreamStatus.Active,
    };
  }

  const map = (val as xdr.ScVal).map();
  const field = (name: string): xdr.ScVal | undefined => {
    for (const entry of map.entries()) {
      if (scalar(entry.key()) === name) return entry.val();
    }
    return undefined;
  };
  const num = (name: string): bigint => toBigInt(scalar(field(name)!));
  const bool_ = (name: string): boolean => Boolean(scalar(field(name)!));
  const addr = (name: string): string => addressFromScAddress(field(name)!.address());
  const opt = (name: string): bigint | null => {
    const raw = field(name);
    if (!raw) return null;
    const native = scalar(raw);
    return native === null ? null : toBigInt(native);
  };
  const status = (): StreamStatus => {
    const n = Number(scalar(field('status')!));
    return [0, 1, 2, 3].includes(n) ? (n as StreamStatus) : StreamStatus.Active;
  };

  return {
    id,
    sender: addr('sender'),
    recipient: addr('recipient'),
    token: addr('token'),
    deposited: num('deposited'),
    withdrawn: num('withdrawn'),
    start_time: num('start_time'),
    end_time: num('end_time'),
    cliff_time: num('cliff_time'),
    cancellable: bool_('cancellable'),
    pausable: bool_('pausable'),
    transferable: bool_('transferable'),
    paused_at: opt('paused_at'),
    paused_total: num('paused_total'),
    status: status(),
  };
}

/** Production client backed by a Soroban RPC endpoint. */
export class SorobanPerpetuaClient implements PerpetuaClient {
  readonly rpcUrl: string;
  readonly contractId: string;
  private readonly server: SorobanRpc.Server;
  private readonly contract: Contract;

  constructor(opts: PerpetuaClientOptions) {
    this.rpcUrl = opts.rpcUrl;
    this.contractId = opts.contractId;
    this.server = new SorobanRpc.Server(opts.rpcUrl);
    this.contract = new Contract(opts.contractId, this.server);
  }

  async getStream(id: bigint): Promise<Stream> {
    const result = (await this.contract.call('get_stream', nativeToScVal(id))) as unknown as xdr.ScVal;
    return asStream(result, id);
  }

  async vestedOf(id: bigint): Promise<bigint> {
    return toBigInt(await this.contract.call('vested_of', nativeToScVal(id)));
  }

  async withdrawableOf(id: bigint): Promise<bigint> {
    return toBigInt(await this.contract.call('withdrawable_of', nativeToScVal(id)));
  }

  async refundableOf(id: bigint): Promise<bigint> {
    return toBigInt(await this.contract.call('refundable_of', nativeToScVal(id)));
  }

  async streamCount(): Promise<bigint> {
    return toBigInt(await this.contract.call('stream_count'));
  }

  async ledgerTimestamp(): Promise<bigint> {
    const latest = await this.server.getLatestLedger();
    return BigInt(latest.timestamp);
  }
}

/** Construct a [`SorobanPerpetuaClient`]. */
export function createPerpetuaClient(opts: PerpetuaClientOptions): PerpetuaClient {
  return new SorobanPerpetuaClient(opts);
}