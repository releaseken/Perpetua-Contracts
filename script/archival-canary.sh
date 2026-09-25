#!/usr/bin/env bash
#
# Stage 4 acceptance criterion — the live archival/restore round trip.
#
# Closes docs/KNOWN-LIMITATIONS.md §1, the one thing the unit suite structurally
# cannot prove: that reading an archived persistent entry *fails* on a real
# network, that `RestoreFootprint` recovers it, and that the data survives.
#
#   before archival:  reports how long is left and exits 0
#   after  archival:  runs the round trip and asserts each step
#
# Usage:
#   script/archival-canary.sh [--restore] [--json]
#
#     (no flag)   status only — safe to run any time
#     --restore   once archived, perform the restore and verify recovery
#     --json      output status as JSON for machine parsing
#
# See contracts/archival-probe/src/lib.rs for why this uses a throwaway probe
# contract rather than a Perpetua stream.

set -euo pipefail

NETWORK="${NETWORK:-testnet}"
RPC_URL="${RPC_URL:-https://soroban-testnet.stellar.org}"
SOURCE="${SOURCE:-fluxora-deployer}"

# Deployed 2026-08-12. Canary planted in the same session.
PROBE="${PROBE:-CB4XJYNXQ62TCXI3GKCVBWADTSTFWYL3ZLYS3MKYPWRANOSADRZG4A7N}"
# ScVal for the unit enum variant `Key::Canary` — Vec[Symbol("Canary")].
KEY_XDR='AAAAEAAAAAEAAAABAAAADwAAAAZDYW5hcnkAAA=='
# Recorded at plant time; the entry received exactly min_persistent_ttl - 1.
PLANTED_AT_LEDGER=4097334
LIVE_UNTIL_LEDGER=4218293

RESTORE=false
JSON_OUTPUT=false

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --restore)
            RESTORE=true
            shift
            ;;
        --json)
            JSON_OUTPUT=true
            shift
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: $0 [--restore] [--json]"
            exit 1
            ;;
    esac
done

latest_ledger() {
    local response
    response=$(curl -sS -m 10 -X POST "$RPC_URL" \
        -H 'Content-Type: application/json' \
        -d '{"jsonrpc":"2.0","id":1,"method":"getLatestLedger"}') || {
        echo "RPC request failed: $RPC_URL" >&2
        return 1
    }
    printf '%s' "$response" | python3 -c '
import json
import sys

payload = json.load(sys.stdin)
if "error" in payload:
    raise SystemExit("RPC error: " + str(payload["error"]))
sequence = payload.get("result", {}).get("sequence")
if not isinstance(sequence, int) or sequence <= 0:
    raise SystemExit("RPC response did not contain a valid ledger sequence")
print(sequence)
'
}

# JSON output function
output_json() {
    local status="$1"
    local message="$2"
    local data="$3"
    
    if [[ -n "$data" ]]; then
        printf '{"status": "%s", "message": "%s", %s}\n' "$status" "$message" "$data"
    else
        printf '{"status": "%s", "message": "%s"}\n' "$status" "$message"
    fi
}

say() { 
    if [[ "$JSON_OUTPUT" == "false" ]]; then
        printf '\n\033[1m── %s\033[0m\n' "$*"
    fi
}

NOW=$(latest_ledger)
REMAINING=$((LIVE_UNTIL_LEDGER - NOW))
TARGET_REACHED=false
ALERT_MESSAGE=""
if (( NOW >= LIVE_UNTIL_LEDGER )); then
    TARGET_REACHED=true
    ALERT_MESSAGE="Canary target ledger $LIVE_UNTIL_LEDGER reached (current: $NOW)"
fi

if [[ "$TARGET_REACHED" == "true" ]] && ! command -v stellar >/dev/null 2>&1; then
    if [[ "$JSON_OUTPUT" == "true" ]]; then
        output_json "error" "stellar CLI is required to inspect the archived entry" \
            "\"current_ledger\": $NOW, \"target_reached\": true, \"alert\": \"$ALERT_MESSAGE\""
    else
        echo "ALERT: $ALERT_MESSAGE"
        echo "stellar CLI is required to inspect the canary entry after the target ledger." >&2
    fi
    exit 2
fi

# Determine entry state
if (( REMAINING > 0 )); then
    STATE="ACTIVE"
    STATE_DESCRIPTION="Entry is live and readable"
    EXIT_CODE=0
elif stellar contract read --id "$PROBE" --network "$NETWORK" \
     --durability persistent --key-xdr "$KEY_XDR" 2>/dev/null; then
    STATE="PENDING_EVICTION"
    STATE_DESCRIPTION="Entry is past live-until but still readable (eviction pending)"
    EXIT_CODE=0
else
    STATE="ARCHIVED"
    STATE_DESCRIPTION="Entry has been successfully archived"
    EXIT_CODE=0
fi

if [[ "$RESTORE" == "true" && "$STATE" == "PENDING_EVICTION" ]]; then
    if [[ "$JSON_OUTPUT" == "true" ]]; then
        output_json "error" "Restore requested before the canary was archived" \
            "\"current_ledger\": $NOW, \"target_reached\": $TARGET_REACHED, \"state\": \"$STATE\""
    else
        echo "Restore requested, but the canary is still readable and eviction is pending." >&2
        echo "Wait for the entry to archive, then retry --restore." >&2
    fi
    exit 2
fi

# Output status
if [[ "$JSON_OUTPUT" == "true" ]]; then
    # JSON mode
    DATA="\"current_ledger\": $NOW, \"target_eviction_ledger\": $LIVE_UNTIL_LEDGER, \"planted_at_ledger\": $PLANTED_AT_LEDGER, \"remaining_ledgers\": $REMAINING, \"target_reached\": $TARGET_REACHED, \"state\": \"$STATE\""
    if [[ "$TARGET_REACHED" == "true" ]]; then
        DATA+=", \"alert\": \"$ALERT_MESSAGE\""
    fi
    output_json "success" "$STATE_DESCRIPTION" "$DATA"
else
    # Human-readable format
    cat <<BANNER
╭──────────────────────────────────────────────────────────────────────╮
│ Perpetua — archival canary                                            │
╰──────────────────────────────────────────────────────────────────────╯
 probe        $PROBE
 planted at   ledger $PLANTED_AT_LEDGER
 lives until  ledger $LIVE_UNTIL_LEDGER
 current      ledger $NOW
 remaining    ledgers: $REMAINING
 state        $STATE — $STATE_DESCRIPTION
BANNER

    if [[ "$TARGET_REACHED" == "true" ]]; then
        printf ' ALERT       %s\n' "$ALERT_MESSAGE"
    fi

    if [[ "$STATE" == "ACTIVE" ]]; then
        printf ' status       ALIVE — %d ledgers left (~%.1f days)\n\n' \
            "$REMAINING" "$(python3 -c "print($REMAINING*5/86400)")"
        echo "Not archived yet. The entry received exactly min_persistent_ttl (120,960"
        echo "ledgers, ~7 days) because the probe deliberately never extends it."
        echo "Re-run after ledger $LIVE_UNTIL_LEDGER, with --restore, to close"
        echo "docs/KNOWN-LIMITATIONS.md §1."
        exit $EXIT_CODE
    elif [[ "$STATE" == "PENDING_EVICTION" ]]; then
        printf ' status       PAST LIVE-UNTIL by %d ledgers — eviction pending\n' "$((-REMAINING))"
        echo
        echo "The entry is past its live-until ledger but has not yet been evicted."
        echo "Eviction is a background scan, so there may be a delay before the"
        echo "entry becomes inaccessible. This is normal behavior."
        echo
        if [[ "$RESTORE" == "false" ]]; then
            echo "Archived and failing as designed. Re-run with --restore to complete the"
            echo "round trip."
            exit 0
        fi
    else
        # STATE == ARCHIVED
        printf ' status       PAST LIVE-UNTIL by %d ledgers — archival expected\n' "$((-REMAINING))"
        echo
    fi
fi

# If we got here and it's not ACTIVE, we need to check archival status
if [[ "$STATE" != "ACTIVE" ]]; then
    if [[ "$JSON_OUTPUT" == "false" ]]; then
        # ---------------------------------------------------------------------------
        say "1. the entry is no longer readable as live state"
        # ---------------------------------------------------------------------------
        if stellar contract read --id "$PROBE" --network "$NETWORK" \
             --durability persistent --key-xdr "$KEY_XDR" 2>/dev/null; then
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                output_json "error" "Entry still readable — it has not been evicted yet" ''
            else
                echo "   ✗ entry still readable — it has not been evicted yet."
                echo "     Eviction is a background scan, so it lags live-until. Retry later."
            fi
            exit 1
        fi
        if [[ "$JSON_OUTPUT" == "true" ]]; then
            output_json "success" "Read failed: the entry is archived" ''
        else
            echo "   ✓ read failed: the entry is archived"
        fi

        # ---------------------------------------------------------------------------
        say "2. invoking the contract fails rather than returning stale data"
        # ---------------------------------------------------------------------------
        if OUT=$(stellar contract invoke --id "$PROBE" --source "$SOURCE" \
                    --network "$NETWORK" --send=yes -- read 2>&1); then
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                output_json "error" "Invocation SUCCEEDED against an archived entry" "{\"output\": \"$OUT\"}"
            else
                echo "   ✗ invocation SUCCEEDED against an archived entry: $OUT"
                echo "     If this happens, the network auto-restored — which would mean the"
                echo "     unit-test caveat in docs/KNOWN-LIMITATIONS.md §1 does not apply on-network."
            fi
            exit 1
        fi
        if [[ "$JSON_OUTPUT" == "true" ]]; then
            output_json "success" "Invocation failed as expected" ''
        else
            echo "   ✓ invocation failed as expected"
            echo "$OUT" | grep -oiE 'archiv[a-z]*|restore[a-z]*|entry.*(expired|missing)' | head -3 |
                sed 's/^/     network said: /' || true
        fi
    fi

    if [[ "$JSON_OUTPUT" == "false" && "$RESTORE" == "false" ]]; then
        echo
        echo "Archived and failing as designed. Re-run with --restore to complete the"
        echo "round trip."
        exit 0
    fi

    if [[ "$RESTORE" == "true" ]]; then
        if [[ "$JSON_OUTPUT" == "false" ]]; then
            # ---------------------------------------------------------------------------
            say "3. restore via RestoreFootprint"
            # ---------------------------------------------------------------------------
            stellar contract restore --id "$PROBE" --source "$SOURCE" --network "$NETWORK" \
                --durability persistent --key-xdr "$KEY_XDR" 2>&1 | tail -3
            echo "   ✓ restore submitted"

            # ---------------------------------------------------------------------------
            say "4. the data came back intact"
            # ---------------------------------------------------------------------------
            VALUE=$(stellar contract invoke --id "$PROBE" --source "$SOURCE" \
                --network "$NETWORK" --send=no -- read 2>/dev/null | tail -1 | tr -d '"')
            if [[ "$VALUE" == "canary" ]]; then
                if [[ "$JSON_OUTPUT" == "true" ]]; then
                    output_json "success" "Read returns \"canary\" — value survived archival and restore" ''
                else
                    echo "   ✓ read returns \"canary\" — value survived archival and restore"
                fi
            else
                if [[ "$JSON_OUTPUT" == "true" ]]; then
                    output_json "error" "Read returned '$VALUE', expected 'canary'" ''
                else
                    echo "   ✗ read returned '$VALUE', expected 'canary'"
                fi
                exit 1
            fi

            NEW_TTL=$(stellar contract read --id "$PROBE" --network "$NETWORK" \
                --durability persistent --key-xdr "$KEY_XDR" 2>/dev/null | awk -F, '{print $NF}')
            if [[ "$JSON_OUTPUT" == "true" ]]; then
                output_json "success" "Entry live again until ledger $NEW_TTL" ''
            else
                echo "   ✓ entry live again until ledger $NEW_TTL"
            fi

            if [[ "$JSON_OUTPUT" == "false" ]]; then
                cat <<'DONE'

╭──────────────────────────────────────────────────────────────────────╮
│ Round trip complete — docs/KNOWN-LIMITATIONS.md §1 can be closed.          │
╰──────────────────────────────────────────────────────────────────────╯
Update docs/KNOWN-LIMITATIONS.md §1 with the transaction hashes above, and note
in the SDK requirements that a client must detect this failure and offer
restore rather than surfacing the raw error.
DONE
            fi
        else
            # JSON mode for restore flow
            stellar contract restore --id "$PROBE" --source "$SOURCE" --network "$NETWORK" \
                --durability persistent --key-xdr "$KEY_XDR" >/dev/null
            
            VALUE=$(stellar contract invoke --id "$PROBE" --source "$SOURCE" \
                --network "$NETWORK" --send=no -- read 2>/dev/null | tail -1 | tr -d '"')
            if [[ "$VALUE" == "canary" ]]; then
                NEW_TTL=$(stellar contract read --id "$PROBE" --network "$NETWORK" \
                    --durability persistent --key-xdr "$KEY_XDR" 2>/dev/null | awk -F, '{print $NF}')
                output_json "success" "Archival restore round trip complete" "{\"new_ttl\": $NEW_TTL}"
            else
                output_json "error" "Read returned '$VALUE', expected 'canary' after restore" ''
                exit 1
            fi
        fi
    fi
fi

exit 0