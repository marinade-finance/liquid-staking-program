mod common;
use harness::env::sol;

#[test]
fn smoke() {
    let mut f = common::fixture("builtin", 2, 2).unwrap();
    f.check("setup");
    let u = &f.users[0];
    f.m.deposit(&mut f.env, u, sol(50)).unwrap();
    f.check("deposit");
    println!("msol {}", f.env.token_balance(&f.users[0].msol));
}
