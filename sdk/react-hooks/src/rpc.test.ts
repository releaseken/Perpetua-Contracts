import { describe, expect, it, vi } from 'vitest';
import { StrKey } from '@stellar/stellar-sdk';
import { batchGetStreams, MAX_BATCH_LOOKUP } from './rpc.js';

describe('batchGetStreams', () => {
  it('checks health, batches ledger entries, and returns a stream map', async () => {
    const streamIds = [7n, 9n];
    const contractId = StrKey.encodeContract(new Uint8Array(32).fill(0x11));

    const makeStreamValue = (id: bigint) => ({
      sender: 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
      recipient: 'GBAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
      token: 'GCAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA',
      deposited: 100n,
      withdrawn: 0n,
      start_time: 1_700_000_000n,
      end_time: 1_700_000_000n + 100n,
      cliff_time: 1_700_000_000n,
      cancellable: true,
      pausable: true,
      transferable: true,
      paused_at: null,
      paused_total: 0n,
      status: 0,
      id,
    });

    const server = {
      getHealth: vi.fn().mockResolvedValue({ status: 'healthy' }),
      getLedgerEntries: vi.fn().mockResolvedValue({
        entries: [
          {
            val: {
              contractData: () => ({
                key: () => ['Stream', 7n],
                val: () => makeStreamValue(7n),
              }),
            },
          },
          {
            val: {
              contractData: () => ({
                key: () => ['Stream', 9n],
                val: () => makeStreamValue(9n),
              }),
            },
          },
        ],
      }),
    };

    const result = await batchGetStreams(server as any, {
      contractId,
      streamIds,
    });

    expect(server.getHealth).toHaveBeenCalledTimes(1);
    expect(server.getLedgerEntries).toHaveBeenCalledTimes(1);
    expect(result).toBeInstanceOf(Map);
    expect(result.size).toBe(2);
    expect(result.get(7n)?.id).toBe(7n);
    expect(result.get(9n)?.id).toBe(9n);
  });

  it('dedupes ids and issues exactly one RPC call per batch', async () => {
    const contractId = StrKey.encodeContract(new Uint8Array(32).fill(0x11));
    const server = {
      getHealth: vi.fn().mockResolvedValue({ status: 'healthy' }),
      getLedgerEntries: vi.fn().mockResolvedValue({ entries: [] }),
    };
    await batchGetStreams(server as any, { contractId, streamIds: [1n, 1n, 2n, 2n, 2n] });
    expect(server.getLedgerEntries).toHaveBeenCalledTimes(1);
    expect(server.getLedgerEntries.mock.calls[0]![0]).toHaveLength(2);
  });

  it('caps batch size and rejects unhealthy RPC; empty is a no-op', async () => {
    const contractId = StrKey.encodeContract(new Uint8Array(32).fill(0x11));
    const server = {
      getHealth: vi.fn().mockResolvedValue({ status: 'unhealthy' }),
      getLedgerEntries: vi.fn(),
    };
    const tooMany = Array.from({ length: MAX_BATCH_LOOKUP + 1 }, (_, i) => BigInt(i));
    await expect(batchGetStreams(server as any, { contractId, streamIds: tooMany })).rejects.toThrow(/batch too large/);
    await expect(batchGetStreams(server as any, { contractId, streamIds: [1n] })).rejects.toThrow(/not healthy/);
    expect((await batchGetStreams(server as any, { contractId, streamIds: [] })).size).toBe(0);
    expect(server.getLedgerEntries).not.toHaveBeenCalled();
  });

  it('stays within the maximum batch under a time budget', async () => {
    const contractId = StrKey.encodeContract(new Uint8Array(32).fill(0x11));
    const server = {
      getHealth: vi.fn().mockResolvedValue({ status: 'healthy' }),
      getLedgerEntries: vi.fn().mockResolvedValue({ entries: [] }),
    };
    const ids = Array.from({ length: MAX_BATCH_LOOKUP }, (_, i) => BigInt(i));
    const t = performance.now();
    await batchGetStreams(server as any, { contractId, streamIds: ids });
    expect(performance.now() - t).toBeLessThan(2000);
    expect(server.getLedgerEntries.mock.calls[0]![0]).toHaveLength(MAX_BATCH_LOOKUP);
  });

  it('retries transient failures idempotently up to maxRetries', async () => {
    const contractId = StrKey.encodeContract(new Uint8Array(32).fill(0x11));
    let attempts = 0;
    const server = {
      getHealth: vi.fn().mockResolvedValue({ status: 'healthy' }),
      getLedgerEntries: vi.fn().mockImplementation(async () => {
        attempts++;
        if (attempts < 3) {
          throw new Error('503 Service Unavailable');
        }
        return { entries: [] };
      }),
    };

    const res = await batchGetStreams(server as any, {
      contractId,
      streamIds: [1n],
      retryOptions: { maxRetries: 3, baseDelayMs: 5 },
    });
    expect(attempts).toBe(3);
    expect(res).toBeInstanceOf(Map);
  });

  it('fails fast on non-retryable errors without retrying', async () => {
    const contractId = StrKey.encodeContract(new Uint8Array(32).fill(0x11));
    let attempts = 0;
    const server = {
      getHealth: vi.fn().mockResolvedValue({ status: 'healthy' }),
      getLedgerEntries: vi.fn().mockImplementation(async () => {
        attempts++;
        throw new Error('invalid contract address');
      }),
    };

    await expect(
      batchGetStreams(server as any, {
        contractId,
        streamIds: [1n],
        retryOptions: { maxRetries: 3, baseDelayMs: 5 },
      })
    ).rejects.toThrow('invalid contract address');
    expect(attempts).toBe(1);
  });
});

