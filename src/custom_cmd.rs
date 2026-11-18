//! Running a custom context-menu entry (U4). The entry's `command` is the
//! program, its `args` a command line that is split into separate arguments
//! FIRST; the `{path}` tokens are replaced in each argument AFTER, so a file
//! name can never add, split or remove an argument (a name with spaces, quotes,
//! `&` or `%` stays one argument). The argument vector goes to
//! `std::process::Command`, which quotes each one for CreateProcess; no shell
//! (`cmd /c`) and no command-line string is ever built by hand.
//!
//! Tokens, in `args`:
//! - `{path}` / `%1`: the first selected item (the folder itself for a click on the background)
//! - `{selection}` / `%*`: every selected item; as a whole argument it becomes one argument per item
//! - `{dir}`: the folder shown in the tab
//! - `{name}`, `{ext}`, `{parent}`: of the first item
//!
//! Spawning (`spawn`) runs on its own short thread: CreateProcess can block on a
//! slow disk or a virus scanner, and the UI thread never waits on that.

use tsubame_config::MenuEntry;

/// What the entry was run on.
pub struct Ctx<'a> {
    /// the selected items (empty on the background)
    pub paths: &'a [String],
    /// the folder shown in the tab
    pub dir: &'a str,
}

/// A program and its arguments, ready to spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argv {
    pub program: String,
    pub args: Vec<String>,
    /// working directory ("" = inherit)
    pub dir: String,
}

/// Splits a command line like a person reads it: spaces separate, `"..."`
/// groups (and may be empty), `\"` inside quotes is a quote. Nothing is
/// expanded here (no `%VAR%`, no globbing).
pub fn split_args(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut quoted = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if quoted {
        return Err("a quote is not closed".into());
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

fn file_name(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

fn parent_of(p: &str) -> &str {
    p.rfind(['\\', '/']).map(|i| &p[..i]).unwrap_or("")
}

fn ext_of(p: &str) -> &str {
    let n = file_name(p);
    n.rfind('.').filter(|&i| i > 0).map(|i| &n[i + 1..]).unwrap_or("")
}

/// Replaces the tokens of one argument. A lone `{selection}` / `%*` gives one
/// argument per item (none when nothing is selected); everything else gives one.
fn expand(arg: &str, ctx: &Ctx) -> Vec<String> {
    if arg == "{selection}" || arg == "%*" {
        return if ctx.paths.is_empty() { vec![ctx.dir.to_string()] } else { ctx.paths.to_vec() };
    }
    let first = ctx.paths.first().map(String::as_str).unwrap_or(ctx.dir);
    let all = if ctx.paths.is_empty() { ctx.dir.to_string() } else { ctx.paths.join(" ") };
    let mut out = String::with_capacity(arg.len() + 16);
    let mut rest = arg;
    while let Some(i) = rest.find(['{', '%']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let (rep, len): (Option<String>, usize) = if tail.starts_with("%1") {
            (Some(first.to_string()), 2)
        } else if tail.starts_with("%*") {
            (Some(all.clone()), 2)
        } else if let Some(end) = tail.find('}').filter(|_| tail.starts_with('{')) {
            let rep = match &tail[1..end] {
                "path" => Some(first.to_string()),
                "selection" => Some(all.clone()),
                "dir" => Some(ctx.dir.to_string()),
                "name" => Some(file_name(first).to_string()),
                "ext" => Some(ext_of(first).to_string()),
                "parent" => Some(parent_of(first).to_string()),
                _ => None,
            };
            (rep, end + 1)
        } else {
            (None, 1)
        };
        match rep {
            Some(r) => out.push_str(&r),
            None => out.push_str(&tail[..len]),
        }
        rest = &tail[len..];
    }
    out.push_str(rest);
    vec![out]
}

/// The argument vector for `entry` run on `ctx`; `Err` says why it cannot run.
pub fn build_argv(entry: &MenuEntry, ctx: &Ctx) -> Result<Argv, String> {
    let program = entry.command.trim().trim_matches('"').to_string();
    if program.is_empty() {
        return Err(format!("\"{}\" has no program set", entry.name));
    }
    if program.contains('\0') {
        return Err("the program name is not valid".into());
    }
    let mut args = Vec::new();
    for a in split_args(&entry.args)? {
        args.extend(expand(&a, ctx));
    }
    if args.iter().any(|a| a.contains('\0')) {
        return Err("a file name is not valid".into());
    }
    Ok(Argv { program, args, dir: ctx.dir.to_string() })
}

/// Starts the program on a short thread. `done` gets `None` when it started
/// and the error text when it did not (the caller shows it in the status bar).
pub fn spawn(argv: Argv, done: impl FnOnce(Option<String>) + Send + 'static) {
    let spawned = std::thread::Builder::new().name("custom-entry".into()).spawn(move || {
        let mut cmd = std::process::Command::new(&argv.program);
        cmd.args(&argv.args);
        if !argv.dir.is_empty() && std::path::Path::new(&argv.dir).is_dir() {
            cmd.current_dir(&argv.dir);
        }
        match cmd.spawn() {
            Ok(child) => {
                drop(child);
                done(None);
            }
            Err(e) => done(Some(format!("Could not start {}: {e}", file_name(&argv.program)))),
        }
    });
    let _ = spawned;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cmd: &str, args: &str) -> MenuEntry {
        MenuEntry::custom("custom.1", "Test", cmd, args)
    }
    fn run(cmd: &str, args: &str, paths: &[&str], dir: &str) -> Argv {
        let p: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        build_argv(&entry(cmd, args), &Ctx { paths: &p, dir }).unwrap()
    }

    #[test]
    fn args_split_like_a_command_line() {
        assert_eq!(split_args("a b  c").unwrap(), ["a", "b", "c"]);
        assert_eq!(split_args(r#"--open "C:\Program Files\x" y"#).unwrap(), ["--open", r"C:\Program Files\x", "y"]);
        assert_eq!(split_args(r#""" a"#).unwrap(), ["", "a"], "an empty quoted argument stays");
        assert_eq!(split_args(r#""say \"hi\"""#).unwrap(), [r#"say "hi""#]);
        assert_eq!(split_args("").unwrap(), Vec::<String>::new());
        assert!(split_args(r#""open"#).is_err());
        assert_eq!(split_args("--x=\"a b\"").unwrap(), ["--x=a b"]);
    }

    #[test]
    fn a_path_with_spaces_stays_one_argument() {
        let a = run("code.exe", r#"--reuse-window "{path}""#, &[r"C:\My Docs\a b.txt"], r"C:\My Docs");
        assert_eq!(a.args, ["--reuse-window", r"C:\My Docs\a b.txt"]);
        // quotes in the entry are optional: the token is its own argument either way
        let a = run("code.exe", "{path}", &[r"C:\My Docs\a b.txt"], r"C:\My Docs");
        assert_eq!(a.args, [r"C:\My Docs\a b.txt"]);
    }

    #[test]
    fn shell_characters_in_names_are_just_characters() {
        let nasty = r#"C:\x\a&b "c" %PATH% ^d |e >f (g) ; h'.txt"#;
        let a = run("tool.exe", "{path} --flag", &[nasty], r"C:\x");
        assert_eq!(a.args, [nasty, "--flag"], "no splitting, no expansion, no extra argument");
        let a = run("tool.exe", "--name={name} --dir={dir}", &[nasty], r"C:\x");
        assert_eq!(a.args, ["--name=a&b \"c\" %PATH% ^d |e >f (g) ; h'.txt".to_string(), r"--dir=C:\x".to_string()]);
    }

    #[test]
    fn a_name_that_looks_like_a_token_is_not_expanded_again() {
        let a = run("t.exe", "{path}", &[r"C:\x\{dir}.txt"], r"C:\x");
        assert_eq!(a.args, [r"C:\x\{dir}.txt"]);
        let a = run("t.exe", "{path}", &[r"C:\x\100%1.txt"], r"C:\x");
        assert_eq!(a.args, [r"C:\x\100%1.txt"]);
    }

    #[test]
    fn selection_is_one_argument_per_item_when_alone() {
        let a = run("t.exe", "--add {selection}", &[r"C:\a b", r"C:\c"], r"C:\");
        assert_eq!(a.args, ["--add", r"C:\a b", r"C:\c"]);
        let a = run("t.exe", "%*", &[r"C:\a", r"C:\b"], r"C:\");
        assert_eq!(a.args, [r"C:\a", r"C:\b"]);
        // inside a larger argument they are joined with spaces (one argument)
        let a = run("t.exe", "--list={selection}", &[r"C:\a", r"C:\b"], r"C:\");
        assert_eq!(a.args, [r"--list=C:\a C:\b"]);
    }

    #[test]
    fn background_click_uses_the_folder() {
        let a = run("t.exe", "{path} {dir}", &[], r"D:\Proj");
        assert_eq!(a.args, [r"D:\Proj", r"D:\Proj"]);
        let a = run("t.exe", "{selection}", &[], r"D:\Proj");
        assert_eq!(a.args, [r"D:\Proj"]);
        assert_eq!(a.dir, r"D:\Proj");
    }

    #[test]
    fn name_ext_parent_tokens() {
        let a = run("t.exe", "{name} {ext} {parent}", &[r"C:\Users\me\photo.final.JPG"], r"C:\Users\me");
        assert_eq!(a.args, ["photo.final.JPG", "JPG", r"C:\Users\me"]);
        let a = run("t.exe", "{ext}|{name}", &[r"C:\x\.gitignore"], r"C:\x");
        assert_eq!(a.args, ["|.gitignore"], "a dot file has no extension");
    }

    #[test]
    fn unknown_tokens_and_stray_braces_pass_through() {
        let a = run("t.exe", "{nope} 100% {", &[r"C:\a"], r"C:\");
        assert_eq!(a.args, ["{nope}", "100%", "{"]);
        let a = run("t.exe", r#"--json "{\"k\": 1}""#, &[], r"C:\");
        assert_eq!(a.args, ["--json", r#"{"k": 1}"#]);
    }

    #[test]
    fn programs_with_spaces_need_no_quotes_and_quotes_are_tolerated() {
        assert_eq!(run(r"C:\Program Files\Git\git-bash.exe", "", &[], r"C:\").program, r"C:\Program Files\Git\git-bash.exe");
        assert_eq!(run(r#""C:\Program Files\Git\git-bash.exe""#, "", &[], r"C:\").program, r"C:\Program Files\Git\git-bash.exe");
    }

    #[test]
    fn bad_entries_say_why() {
        let ctx = Ctx { paths: &[], dir: r"C:\" };
        assert!(build_argv(&entry("", "x"), &ctx).unwrap_err().contains("no program"));
        assert!(build_argv(&entry("t.exe", "\"open"), &ctx).is_err());
        let p = vec!["a\0b".to_string()];
        assert!(build_argv(&entry("t.exe", "{path}"), &Ctx { paths: &p, dir: "" }).is_err());
    }

    #[test]
    fn never_a_shell() {
        // the argument vector for a name full of shell syntax has the same
        // length as the entry's tokens, whatever the name holds
        let evil = r#"x" & calc & ""#;
        let a = run("t.exe", "{path} {dir}", &[evil], r"C:\x");
        assert_eq!(a.args.len(), 2);
        assert_eq!(a.program, "t.exe");
    }

    #[test]
    fn spawn_reports_a_missing_program_without_blocking() {
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::time::Instant::now();
        spawn(Argv { program: r"C:\definitely\not\here.exe".into(), args: vec![], dir: String::new() }, move |r| {
            let _ = tx.send(r);
        });
        assert!(t.elapsed() < std::time::Duration::from_millis(100), "the caller does not wait");
        let r = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(r.unwrap().contains("Could not start"));
    }
}
