//! Sequence fuzzing: long mostly-valid chains (deposit, stake, epochs, rewards, slashing, unstake, claim) with rare mutation.
//! Restrict with FUZZ_ONLY=deposit,claim ; mutation rate with FUZZ_MUTATE_PCT.
use mfuzz::engine::{self, Fx, Mode};
use mfuzz::util::*;

#[derive(FuzzTestMethods)]
struct FuzzTest {
    trident: Trident,
    fuzz_accounts: Fx,
}

#[flow_executor]
impl FuzzTest {
    fn new() -> Self {
        let mut trident = Trident::default();
        engine::install_panic_recorder();
        mfuzz::native::deploy(&mut trident);
        Self { trident, fuzz_accounts: Fx::default() }
    }

    #[init]
    fn start(&mut self) {
        engine::setup(&mut self.trident, &mut self.fuzz_accounts);
    }

    #[flow]
    fn step(&mut self) {
        engine::step(&mut self.trident, &mut self.fuzz_accounts, Mode::Flow);
    }

    #[end]
    fn end(&mut self) {
        engine::flush_stats("fuzz_flows", false);
    }
}

fn main() {
    let iterations = std::env::var("FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(2_000);
    let calls = std::env::var("FUZZ_CALLS").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    FuzzTest::fuzz(iterations, calls);
    engine::flush_stats("fuzz_flows", true);
}
