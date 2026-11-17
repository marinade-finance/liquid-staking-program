//! Account-set mutation and the oracle "this substitution must NOT succeed".
//!
//! A mutated instruction is compared slot by slot against the valid one it came from, so several
//! stacked mutations (swap + dup + unsign ...) are judged on the final account list only.

use crate::ixs::{Ix, Role};
use crate::util::*;
use crate::world::World;

#[derive(Clone, Debug)]
pub struct Mutation {
    pub slot: usize,
    pub kind: &'static str,
    pub detail: String,
}

fn rnd(t: &mut Trident, n: usize) -> usize {
    t.random_from_range(0..n)
}

/// key -> signer-capable? (wallets, authorities, users; not PDAs, mints, programs)
fn can_sign(w: &World, t: &mut Trident, k: &Pubkey) -> bool {
    w.tags_of(t, k).contains(&"wallet")
}

pub fn mutate(w: &mut World, t: &mut Trident, ix: &mut Ix, count: usize) -> Vec<Mutation> {
    let mut out = vec![];
    for _ in 0..count {
        if ix.slots.is_empty() {
            break;
        }
        let i = rnd(t, ix.slots.len());
        let orig = ix.slots[i].meta.clone();
        let kind = match rnd(t, 21) {
            0..=1 => "random_key",
            2..=5 => "pool_key",
            6..=8 => "dup_slot",
            9 => "attacker",
            10..=11 => "unsign",
            12..=13 => "clone_foreign_owner",
            14..=15 => "clone_lookalike",
            16..=18 => "swap_slots",
            19 => "readonly",
            _ => "stake_or_vote_decoy",
        };
        let mut detail = String::new();
        match kind {
            "random_key" => {
                let k = t.random_pubkey();
                set_key(w, t, ix, i, k);
                detail = format!("-> fresh {k}");
            }
            "pool_key" => {
                let k = w.pool[rnd(t, w.pool.len())];
                set_key(w, t, ix, i, k);
                detail = format!("-> pool {k} {:?}", w.tags_of(t, &k));
            }
            "dup_slot" => {
                let j = rnd(t, ix.slots.len());
                let k = ix.slots[j].meta.pubkey;
                set_key(w, t, ix, i, k);
                detail = format!("-> dup of slot {} ({k})", ix.slots[j].name);
            }
            "attacker" => {
                let k = w.attacker.key;
                set_key(w, t, ix, i, k);
                detail = format!("-> attacker {k}");
            }
            "unsign" => {
                if orig.is_signer {
                    ix.slots[i].meta.is_signer = false;
                } else {
                    // no-op for non-signers: pick a signer slot instead, if any
                    if let Some(j) = ix.slots.iter().position(|s| s.meta.is_signer) {
                        ix.slots[j].meta.is_signer = false;
                        out.push(Mutation { slot: j, kind, detail: "signer flag dropped".into() });
                    }
                    continue;
                }
            }
            "clone_foreign_owner" | "clone_lookalike" => {
                let acc = t.get_account(&orig.pubkey);
                let k = t.random_pubkey();
                let lookalike = kind == "clone_lookalike";
                let owner = if lookalike {
                    *acc.owner()
                } else {
                    match rnd(t, 4) {
                        0 => system_id(),
                        1 => token_id(),
                        2 => stake_id(),
                        _ => t.random_pubkey(),
                    }
                };
                let o = AccountSharedData::create(acc.lamports(), acc.data().to_vec(), owner, false, u64::MAX);
                t.set_account_custom(&k, &o);
                let tags = if lookalike { w.tags_of(t, &orig.pubkey) } else if owner == system_id() && acc.data().is_empty() { vec!["wallet"] } else { vec![] };
                w.tags.insert(k, tags);
                w.track(t, k);
                set_key(w, t, ix, i, k);
                detail = format!("-> {kind} of {} (owner {owner})", orig.pubkey);
            }
            "swap_slots" => {
                let j = rnd(t, ix.slots.len());
                if i != j {
                    let (a, b) = (ix.slots[i].meta.clone(), ix.slots[j].meta.clone());
                    // keys travel, the position-bound flags (writable / signer) stay with the position
                    // unless the key cannot sign
                    set_key(w, t, ix, i, b.pubkey);
                    set_key(w, t, ix, j, a.pubkey);
                    detail = format!("<-> slot {}", ix.slots[j].name);
                    out.push(Mutation { slot: j, kind, detail: format!("<-> slot {}", ix.slots[i].name) });
                }
            }
            "readonly" => {
                if let Some(j) = ix.slots.iter().position(|s| s.meta.is_writable) {
                    let j = if ix.slots[i].meta.is_writable { i } else { j };
                    ix.slots[j].meta.is_writable = false;
                    out.push(Mutation { slot: j, kind, detail: "writable flag dropped".into() });
                }
                continue;
            }
            _ => {
                // stake / vote flavoured decoys: pick pool entries that carry stake or vote tags
                let cands: Vec<Pubkey> = w
                    .pool
                    .iter()
                    .copied()
                    .filter(|k| w.tags.get(k).map(|v| v.iter().any(|x| x.contains("stake") || x.contains("vote"))).unwrap_or(false))
                    .collect();
                if cands.is_empty() {
                    continue;
                }
                let k = cands[rnd(t, cands.len())];
                set_key(w, t, ix, i, k);
                detail = format!("-> decoy {k} {:?}", w.tags_of(t, &k));
            }
        }
        out.push(Mutation { slot: i, kind, detail });
    }
    out
}

/// replace the key of slot i; the signer flag survives only if the new key could sign
fn set_key(w: &World, t: &mut Trident, ix: &mut Ix, i: usize, k: Pubkey) {
    let signer = ix.slots[i].meta.is_signer;
    ix.slots[i].meta.pubkey = k;
    if signer && !can_sign(w, t, &k) {
        ix.slots[i].meta.is_signer = false;
    }
}

/// `Some(reason)` if a success of `fin` (derived from `orig`) would prove a missing check.
pub fn must_fail(w: &World, t: &mut Trident, orig: &Ix, fin: &Ix) -> Option<String> {
    for (i, (o, f)) in orig.slots.iter().zip(&fin.slots).enumerate() {
        if o.meta.pubkey != f.meta.pubkey {
            let ft = w.tags_of(t, &f.meta.pubkey);
            let desc = format!("slot #{i} `{}` ({:?}) {} -> {} tags {:?}", o.name, o.role, o.meta.pubkey, f.meta.pubkey, ft);
            match o.role {
                Role::Fixed | Role::Auth => return Some(format!("{} accepted a different account: {desc}", fin.name)),
                Role::New => {
                    if !ft.iter().any(|x| *x == "wallet") {
                        return Some(format!("{} accepted a non-wallet as new account: {desc}", fin.name));
                    }
                }
                Role::Free => {
                    if !ft.iter().any(|x| o.accepts.contains(x)) {
                        return Some(format!("{} accepted wrong-kind account (accepts {:?}): {desc}", fin.name, o.accepts));
                    }
                }
            }
        }
        if o.meta.is_signer && !f.meta.is_signer && !fin.slots.iter().any(|s| s.meta.pubkey == f.meta.pubkey && s.meta.is_signer) {
            return Some(format!("{} succeeded without signer on slot #{i} `{}` ({})", fin.name, o.name, f.meta.pubkey));
        }
        if o.meta.is_writable && !f.meta.is_writable && !fin.slots.iter().any(|s| s.meta.pubkey == f.meta.pubkey && s.meta.is_writable) {
            return Some(format!("{} succeeded with slot #{i} `{}` read-only ({})", fin.name, o.name, f.meta.pubkey));
        }
    }
    None
}
