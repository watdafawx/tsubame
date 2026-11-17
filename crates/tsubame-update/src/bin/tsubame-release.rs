//! `tsubame-release`: maintainer tool (feature `release-tool`). Never shipped to users.
//!
//!   tsubame-release keygen --out <file>             new keypair: seed -> <file>, prints the PUBLIC key only
//!   tsubame-release pubkey (--key-file F | --key-env VAR | --dev-test-key)
//!   tsubame-release sign   --manifest M --out SIG (--key-file F | --key-env VAR | --dev-test-key) [--allow-unpinned]
//!   tsubame-release verify --manifest M --sig SIG [--dev-test-key]
//!
//! The private key (a 32-byte seed, 64 hex chars) is read from a file or an environment
//! variable and is never printed, logged or written anywhere else. `sign` refuses a key
//! whose public half is not pinned in `keys.rs` (a release the updater would reject),
//! unless `--allow-unpinned` or `--dev-test-key` is given, and always verifies its own output.

use ed25519_compact::{KeyPair, Seed};
use tsubame_update::{hash, keys};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn val(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned()
}

fn load_key(args: &[String]) -> (KeyPair, bool) {
    if args.iter().any(|a| a == "--dev-test-key") {
        return (tsubame_update::devkey::dev_keypair(), true);
    }
    let text = if let Some(f) = val(args, "--key-file") {
        std::fs::read_to_string(&f).unwrap_or_else(|e| die(format!("cannot read key file {f}: {e}")))
    } else if let Some(v) = val(args, "--key-env") {
        std::env::var(&v).unwrap_or_else(|_| die(format!("environment variable {v} is not set")))
    } else {
        die("need --key-file, --key-env or --dev-test-key")
    };
    let seed_bytes = hash::from_hex(text.trim()).unwrap_or_else(|_| die("key is not hex"));
    let seed = Seed::from_slice(&seed_bytes).unwrap_or_else(|_| die("key must be a 32-byte seed (64 hex chars)"));
    (KeyPair::from_seed(seed), false)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        eprintln!("usage: tsubame-release <keygen|pubkey|sign|verify> ...");
        std::process::exit(2);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "keygen" => {
            let out = val(rest, "--out").unwrap_or_else(|| die("keygen needs --out <file>"));
            if std::path::Path::new(&out).exists() {
                die(format!("{out} already exists; refusing to overwrite a key"));
            }
            let kp = KeyPair::generate();
            let seed_hex = hash::to_hex(kp.sk.seed().as_ref());
            std::fs::write(&out, format!("{seed_hex}\n")).unwrap_or_else(|e| die(format!("cannot write {out}: {e}")));
            println!("private key written to {out} (keep it secret, never commit it)");
            println!("public key (paste into RELEASE_PUBLIC_KEYS_HEX in crates/tsubame-update/src/keys.rs):");
            println!("{}", hash::to_hex(kp.pk.as_ref()));
        }
        "pubkey" => {
            let (kp, _) = load_key(rest);
            println!("{}", hash::to_hex(kp.pk.as_ref()));
        }
        "sign" => {
            let mpath = val(rest, "--manifest").unwrap_or_else(|| die("sign needs --manifest"));
            let out = val(rest, "--out").unwrap_or_else(|| die("sign needs --out"));
            let (kp, is_dev) = load_key(rest);
            let pub_hex = hash::to_hex(kp.pk.as_ref());
            let pinned = keys::RELEASE_PUBLIC_KEYS_HEX.iter().any(|k| k.eq_ignore_ascii_case(&pub_hex));
            if !pinned && !is_dev && !rest.iter().any(|a| a == "--allow-unpinned") {
                die(format!("public key {pub_hex} is not in RELEASE_PUBLIC_KEYS_HEX: installed apps would reject this release (pin it, or pass --allow-unpinned)"));
            }
            let manifest = std::fs::read(&mpath).unwrap_or_else(|e| die(format!("cannot read {mpath}: {e}")));
            let sig = kp.sk.sign(keys::signed_message(&manifest), None);
            let sig_hex = hash::to_hex(sig.as_ref());
            // self-check against the public half just derived
            if kp.pk.verify(keys::signed_message(&manifest), &sig).is_err() {
                die("self-verification failed");
            }
            std::fs::write(&out, format!("{sig_hex}\n")).unwrap_or_else(|e| die(format!("cannot write {out}: {e}")));
            println!("signed {mpath} with public key {pub_hex}{}", if is_dev { " (DEV TEST KEY: not trusted by release builds)" } else { "" });
        }
        "verify" => {
            let mpath = val(rest, "--manifest").unwrap_or_else(|| die("verify needs --manifest"));
            let spath = val(rest, "--sig").unwrap_or_else(|| die("verify needs --sig"));
            let manifest = std::fs::read(&mpath).unwrap_or_else(|e| die(format!("cannot read {mpath}: {e}")));
            let sig = std::fs::read(&spath).unwrap_or_else(|e| die(format!("cannot read {spath}: {e}")));
            let mut ks: Vec<_> = keys::RELEASE_PUBLIC_KEYS_HEX.iter().filter_map(|h| keys::parse_public_key(h).ok()).collect();
            if rest.iter().any(|a| a == "--dev-test-key") {
                ks.push(tsubame_update::devkey::dev_public_key());
            }
            match keys::verify_signature(&manifest, &sig, &ks) {
                Ok(()) => println!("signature OK"),
                Err(e) => die(e),
            }
        }
        other => die(format!("unknown command {other:?}")),
    }
}
