# Archival Probe Precision-Safe Fractional Stream Withdrawal Calculation Specification

## Executive Summary
This document addresses **Issue #375**: *fix(contracts/archival-probe): resolve edge-case precision loss in archival-probe during fractional stream withdrawal calculations*.

The `contracts/archival-probe` subsystem proves persistent storage archival dynamics against live network ledgers without premature TTL refresh. When integrated with or simulating streaming withdrawal scenarios (such as dry-run validations or fractional stream claim verification against archived/restored streams), fractional withdrawal calculations must guarantee exact conservation without truncation dust loss or intermediate integer overflow.

This specification details the mathematical model, defensive error handling, formal invariants, and fuzzing verification implemented to resolve edge-case precision loss during fractional stream withdrawal calculations.

---

## Technical Scope & Mathematical Formulation

### 1. The Precision Loss & Intermediate Overflow Problem
In fractional stream withdrawals, callers specify partial withdrawals as a rational ratio:
$$\text{payout} = \frac{\text{total} \times \text{numerator}}{\text{denominator}}$$

Two distinct edge cases arise in naive implementations:
1. **Truncation Residue & Dust Loss**: Standard integer division floors toward zero. In fractional sweeps (e.g. 50% of 7 stroops), $\lfloor 7 \times 50 / 100 \rfloor = 3$. If the remainder is computed via another multiplication or division, 1 stroop of dust is silently lost or unaccounted for.
2. **Intermediate Integer Overflow**: Evaluating `total * numerator` directly can overflow `i128` even when the final quotient fits within `i128` bounds (e.g., when `total` is near `i128::MAX` with basis point fractions).

### 2. Quotient-Remainder Decomposition
To guarantee arithmetic safety for all balances up to `i128::MAX`, the calculation utilizes quotient-remainder decomposition:
$$\text{total} = q \times d + r \quad (q = \lfloor \text{total} / d \rfloor, \; r = \text{total} \pmod d)$$
$$\left\lfloor \frac{\text{total} \times n}{d} \right\rfloor = q \times n + \left\lfloor \frac{r \times n}{d} \right\rfloor$$

- **Bounded High Component**: $q \times n = (\lfloor \text{total} / d \rfloor) \times n \le \text{total} \times (n / d) \le \text{total} \le \text{i128::MAX}$.
- **Bounded Low Component**: Since $r < d$, the intermediate product $r \times n < d \times n \le d^2$. For standard basis-point scaling ($d = 10,000$), $d^2 = 10^8 \ll \text{i128::MAX}$.
- **Exact Conservation Identity**: The remainder is computed by direct subtraction:
  $$\text{remainder} = \text{total} - \text{payout}$$
  This strictly guarantees:
  $$\text{payout} + \text{remainder} == \text{total}$$
  leaving zero stranded dust in the contract or verification trace.

### 3. Equal-Share Residue Distribution
For multi-party split probes dividing `total` into $n$ equal shares:
- Each share receives base allocation $\lfloor \text{total} / n \rfloor$.
- The remainder $r = \text{total} \pmod n$ is distributed by adding 1 stroop to each of the first $r$ shares.
- The sum over all $n$ shares strictly equals `total`.

---

## Defensive Error Handling & Error Taxonomy

New explicit error discriminants ensure robust operational feedback:

| Discriminant | Name | Cause |
|---|---|---|
| `5` | `Error::ArithmeticOverflow` | Intermediate overflow or negative input amounts. |
| `6` | `Error::ZeroDenominator` | Denominator is non-positive ($d \le 0$). |
| `7` | `Error::InvalidFraction` | Numerator is negative or exceeds denominator ($n < 0 \lor n > d$). |
| `8` | `Error::InvariantViolation` | Conservation identity $P + R == T$ failed validation. |

---

## Formal Invariants

| Invariant | Formal Definition | Enforcement Point |
|---|---|---|
| **I1: Exact Conservation** | $\forall T \ge 0, 0 \le n \le d, d > 0: P + R = T$ | `FractionalSplit::assert_conservation` |
| **I2: Bounded Allocation** | $0 \le P \le T$ and $0 \le R \le T$ | `calculate_fractional_withdrawal` |
| **I3: No Intermediate Overflow** | $\forall T \le \text{i128::MAX}$, decomposition prevents overflow | `calculate_fractional_withdrawal` |
| **I4: Zero Denominator Rejection** | $d \le 0 \implies \text{Err(ZeroDenominator)}$ | Input guard |
| **I5: Persistence Without Extension** | Canary and probe entries never receive TTL extensions | `plant`, `plant_probe`, `batch_plant` |

---

## Verification & Test Matrix

The test suite in `contracts/archival-probe/src/test.rs` validates:
1. **Exact Fractional Conservation**: Verifies odd-balance divisions (e.g. 7 stroops at 50%, 100 stroops at 1/3) maintain $P + R == T$.
2. **Extreme Upper Boundary Safety**: Exercises balances near `i128::MAX` with high fractions without intermediate overflow.
3. **Defensive Rejections**: Tests rejection of $d \le 0$, $n < 0$, $n > d$, $T < 0$, and basis points $> 10,000$.
4. **Equal Share Distribution**: Validates residue allocation across $n \le 16$ shares for prime and composite totals.
5. **Property-Based Fuzz Testing**: Proptest fuzzing across $10^{18}$ balances and arbitrary rational ratios asserting conservation invariants.
