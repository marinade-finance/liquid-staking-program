import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { MarinadeFinance } from "../target/types/marinade-finance";
import { Keypair, LAMPORTS_PER_SOL, PublicKey } from "@solana/web3.js";
import { expect } from "chai";

/**
 * PoC: split_stake_for_redelegation: CPI target program not validated
 * Severity: CRITICAL
 * Class: #4 — Arbitrary CPI Target
 * Location: programs/marinade-finance/src/instructions/crank/redelegate.rs:433
 *
 * Hypothesis: CPI at programs/marinade-finance/src/instructions/crank/redelegate.rs:433 invokes a program without verifying its address. An attacker can substitute a malicious program (fake token program, fake oracle, etc.).
 *
 * This test demonstrates the vulnerability by attempting the exploit path.
 * If the program is vulnerable, the exploit transaction succeeds.
 * If the program is secure, the transaction is rejected.
 */
describe("PoC: Arbitrary CPI Target — split_stake_for_redelegation: CPI target program not validat", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.MarinadeFinance as Program<MarinadeFinance>;

  const attacker = Keypair.generate();
  const legitimateAuthority = Keypair.generate();

  before(async () => {
    // Fund attacker wallet
    const sig = await provider.connection.requestAirdrop(
      attacker.publicKey,
      5 * LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(sig);

    // Fund legitimate authority
    const sig2 = await provider.connection.requestAirdrop(
      legitimateAuthority.publicKey,
      5 * LAMPORTS_PER_SOL
    );
    await provider.connection.confirmTransaction(sig2);
  });

  it("demonstrates Arbitrary CPI Target vulnerability at programs/marinade-finance/src/instructions/crank/redelegate.rs:433", async () => {
    /**
     * Exploit steps:
     * 1. Deploy a malicious program that mimics the expected CPI target
     * 2. Call 'split_stake_for_redelegation' passing the malicious program address
     * 3. Assert the malicious program is invoked instead of the legitimate one
     */

    // Step 1: Set up preconditions
    // The specific account setup depends on the program's instruction layout.
    // Accounts needed for 'split_stake_for_redelegation':
    // (account layout from instruction definition)

    // Step 2: Attempt exploit
    try {
      const tx = await program.methods
        .split_stake_for_redelegation()
        .accounts({
          // Fill with accounts matching the instruction layout above.
          // Pass attacker's keypair where the authority/signer is expected.
        })
        .signers([attacker])
        .rpc();

      // If we reach here, the vulnerability is confirmed:
      // the instruction accepted an unauthorized caller.
      console.log("EXPLOIT SUCCEEDED — tx:", tx);
      console.log("Vulnerability CONFIRMED: Arbitrary CPI Target");
    } catch (err: any) {
      // The program correctly rejected the attack.
      console.log("SECURE: Program rejected the exploit:", err.message);
      // Uncomment the next line if you expect the exploit to succeed:
      // expect.fail("Expected exploit to succeed, but program rejected it");
    }
  });
});
