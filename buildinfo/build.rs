//! Stamps the build info into `$OUT_DIR/buildinfo.rs`. The parsing and git code
//! lives in `src/parse.rs` and `src/collect.rs` (unit-tested by `cargo test -p buildinfo`)
//! and is included here so the tests cover what the build runs.
#![allow(dead_code)]

#[path = "src/collect.rs"]
mod collect;
#[path = "src/parse.rs"]
mod parse;

use std::path::PathBuf;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn hostname() -> String {
    for var in ["COMPUTERNAME", "HOSTNAME"] {
        if let Some(v) = env(var).filter(|v| !v.is_empty()) {
            return v;
        }
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn main() {
    let dir = PathBuf::from(env("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DOCXY_BUILD_KIND");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-env-changed=GITHUB_HEAD_REF");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    for p in collect::watch_paths(&dir, &env) {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    let f = collect::collect(&dir, &env);
    let kind = parse::kind_from_env(env("DOCXY_BUILD_KIND").as_deref());
    let secs = env("SOURCE_DATE_EPOCH")
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .unwrap_or(0);

    let (pr_num, pr_title) = match &f.last_pr {
        Some((n, t)) => (format!("Some({n})"), format!("Some({t:?})")),
        None => ("None".into(), "None".into()),
    };
    let opt = |v: Option<u32>| v.map_or("None".to_string(), |n| format!("Some({n})"));
    let src = format!(
        "Raw {{ commit: {:?}, branch: {:?}, commit_date: {:?}, dirty: {}, last_pr: {}, \
         last_pr_title: {}, issue: {}, ahead: {}, built_at: {:?}, profile: {:?}, target: {:?}, \
         host: {:?}, kind: {:?} }}\n",
        f.commit,
        f.branch,
        f.commit_date,
        f.dirty,
        pr_num,
        pr_title,
        opt(f.issue),
        opt(f.ahead),
        parse::utc_timestamp(secs),
        env("PROFILE").unwrap_or_else(|| "unknown".into()),
        env("TARGET").unwrap_or_else(|| "unknown".into()),
        hostname(),
        kind.as_str(),
    );
    let out = PathBuf::from(env("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("buildinfo.rs"), src).expect("write buildinfo.rs");
}
