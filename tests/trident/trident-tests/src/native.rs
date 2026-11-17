//! Executor selection: native (default, enables llvm coverage of the program source) or the compiled
//! SBF binary (`MARINADE_SBF=/path/marinade_finance.so`).
use crate::util::*;

pub fn deploy(t: &mut Trident) {
    if let Ok(path) = std::env::var("MARINADE_SBF") {
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("MARINADE_SBF={path}: {e}"));
        t.deploy_program(TridentProgram::new(program_id(), None, data));
    } else {
        t.deploy_entrypoint(TridentEntrypoint::new(program_id(), None, processor!(marinade_finance::entry)));
    }
}
