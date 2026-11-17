mod common;
use harness::convert_ix;
use harness::env::*;
use harness::profile::{self, addr, STAKE_ID};
use solana_signer::Signer;

const LEGACY: &str = "builtin,-set_lamports_per_byte_to_6333,-set_lamports_per_byte_to_5080";

#[test]
fn rent_is_read_by_the_stake_program_and_transition_switches_it() {
    let mut env = env_from_spec(LEGACY).unwrap();
    assert_eq!(env.stake_rent(), 2_282_880);
    let vote = env.create_vote_account();
    let auth = env.new_funded(sol(1));
    let s1 = env.create_delegated_stake(&auth, &vote, sol(10)).unwrap();
    let v1 = env.stake_view(&s1).unwrap();
    assert_eq!(v1.delegation.as_ref().unwrap().stake, sol(10));
    assert_eq!(v1.lamports, sol(10) + 2_282_880);
    assert_eq!(v1.rent_exempt_reserve, 2_282_880);

    let target = profile::resolve("builtin").unwrap();
    env.transition(&target).unwrap();
    assert_eq!(env.rent().minimum_balance(200), 1_666_240);
    assert_eq!(env.stake_rent(), 1_666_240);
    let s2 = env.create_delegated_stake(&auth, &vote, sol(10)).unwrap();
    let v2 = env.stake_view(&s2).unwrap();
    // the stake program delegates lamports - LIVE rent, but still freezes the legacy reserve in Meta
    assert_eq!(v2.lamports, sol(10) + 1_666_240);
    assert_eq!(v2.delegation.as_ref().unwrap().stake, sol(10), "stake program must read the live Rent");
    assert_eq!(v2.rent_exempt_reserve, 2_282_880);
}

#[test]
fn transition_swaps_program_binaries() {
    if !harness::tests_dir().join("profiles/mainnet.json").exists() {
        return;
    }
    let mut env = env_from_spec("builtin").unwrap();
    let bundled = env.program_sha256(&addr(STAKE_ID)).unwrap();
    let rt = profile::resolve("profiles/mainnet.json").unwrap();
    let want = rt.programs[&addr(STAKE_ID)].sha256.clone();
    assert_ne!(bundled, want);
    env.transition(&rt).unwrap();
    assert_eq!(env.program_sha256(&addr(STAKE_ID)).unwrap(), want);
    // and it still works
    let vote = env.create_vote_account();
    let auth = env.new_funded(sol(1));
    env.create_delegated_stake(&auth, &vote, sol(5)).unwrap();
}

#[test]
fn feature_toggle_changes_engine_feature_set() {
    let mut env = env_from_spec("builtin").unwrap();
    let id = addr("rntCigrTppP5JdZz7K8TyN9sMzLdAcXp8SejYpVpX6D");
    assert!(!env.svm().get_feature_set_ref().is_active(&id));
    let rt = profile::resolve("builtin,+set_lamports_per_byte_to_2575").unwrap();
    env.transition(&rt).unwrap();
    assert!(env.svm().get_feature_set_ref().is_active(&id));
    assert_eq!(env.stake_rent(), 328 * 2575);
}

#[test]
fn epochs_activation_and_deactivation_follow_the_stake_program() {
    let mut env = env_from_spec("builtin").unwrap();
    let vote = env.create_vote_account();
    let auth = env.new_funded(sol(1));
    let s = env.create_delegated_stake(&auth, &vote, sol(10)).unwrap();
    let d = env.stake_view(&s).unwrap().delegation.unwrap();
    assert_eq!(env.activation(&d), Activation::Activating);
    let e0 = env.epoch();
    let c0 = env.clock();
    env.warp_epochs(1);
    let c1 = env.clock();
    assert_eq!(c1.epoch, e0 + 1);
    assert!(c1.unix_timestamp - c1.epoch_start_timestamp >= 1800);
    assert!(c1.unix_timestamp > c0.unix_timestamp);
    assert_eq!(env.activation(&d), Activation::FullyActive);

    // deactivate through the real program, then the full balance is withdrawable one epoch later
    let ix = solana_stake_interface::instruction::deactivate_stake(&p(&s), &p(&auth.pubkey()));
    env.send(&[convert_ix!(&ix)], &[&auth]).unwrap();
    let d = env.stake_view(&s).unwrap().delegation.unwrap();
    assert_eq!(env.activation(&d), Activation::Deactivating);
    let lamports = env.lamports(&s);
    let wd = |env: &mut TestEnv, amount: u64| {
        let ix = solana_stake_interface::instruction::withdraw(&p(&s), &p(&auth.pubkey()), &p(&auth.pubkey()), amount, None);
        env.send(&[convert_ix!(&ix)], &[&auth])
    };
    assert!(wd(&mut env, lamports).is_err(), "still cooling down in the deactivation epoch");
    env.warp_epochs(1);
    assert_eq!(env.activation(&d), Activation::Deactivated);
    wd(&mut env, lamports).unwrap();
}

#[test]
fn rewards_and_mev_helpers() {
    let mut env = env_from_spec("builtin").unwrap();
    let vote = env.create_vote_account();
    let auth = env.new_funded(sol(1));
    let s = env.create_delegated_stake(&auth, &vote, sol(10)).unwrap();
    let before = env.stake_view(&s).unwrap();
    env.reward_stake(&s, 1000);
    env.add_mev(&s, 77);
    let after = env.stake_view(&s).unwrap();
    assert_eq!(after.delegation.unwrap().stake, before.delegation.unwrap().stake + 1000);
    assert_eq!(after.lamports, before.lamports + 1077);
    env.set_epoch_rewards_active(true);
    let ix = solana_stake_interface::instruction::deactivate_stake(&p(&s), &p(&auth.pubkey()));
    assert!(env.send(&[convert_ix!(&ix)], &[&auth]).is_err());
}
