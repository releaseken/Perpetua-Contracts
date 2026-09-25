import { describe, expect, it, vi } from 'vitest';
import { StrKey } from '@stellar/stellar-sdk';
import { batchGetStreams } from './rpc.js';

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
});
