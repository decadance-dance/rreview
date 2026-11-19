//! The fake reviewers' scenario. rreview passes a reviewer only the variables it inherits
//! (spec §3), so the fake `claude` and `codex` read their `FAKE_*` variables from
//! `<bin>/fake.env`, next to them; `gh` and `orca` still get rreview's environment.

use std::fs;
use std::path::Path;

pub fn write(bin: &Path, vars: &[(&str, String)]) {
    let mut s = String::new();
    for (k, v) in vars.iter().filter(|(k, _)| k.starts_with("FAKE_")) {
        s += &format!("export {k}='{}'\n", v.replace('\'', r"'\''"));
    }
    // Runs that share `bin` at once each replace the whole file.
    let tmp = bin.join(format!("fake.env.{:?}", std::thread::current().id()));
    fs::write(&tmp, s).unwrap();
    fs::rename(&tmp, bin.join("fake.env")).unwrap();
}
