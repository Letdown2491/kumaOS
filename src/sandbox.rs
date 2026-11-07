//! `kuma run`: a sandbox for the scripts a build tool runs behind your
//! back.
//!
//! npm executes lifecycle scripts on every install, and pip's own docs
//! answer "running arbitrary code from distributions" when asked what
//! an install does. A project's directory is yours to give a build
//! tool; your home directory is not necessarily theirs. This verb runs
//! a command inside bubblewrap — unprivileged, no setuid bit, already
//! in every kuma image — with a file system the command can read and a
//! write surface you chose:
//!
//! - `/` is bound read-only, `/dev` and `/proc` are fresh, `/tmp` is a
//!   tmpfs that dies with the sandbox.
//! - `$HOME` is a tmpfs: empty when the child looks, gone when it
//!   exits. The caches and tokens a package manager would reach for
//!   (`~/.npmrc`, `~/.cache/pip`, the browser profile a postinstall
//!   might read) are not in the sandbox to find.
//! - The working directory is bound read-write **after** the tmpfs
//!   home — later mounts win, which is the whole trick — so a project
//!   under the home directory is the one real, writable thing the
//!   child sees. The cwd is the deal: `run` exists so `npm install`
//!   can build the thing you are in.
//! - The agent sockets the environment names (`SSH_AUTH_SOCK`,
//!   `GPG_AGENT_INFO`) are unset: a script that cannot see your home
//!   has no business holding your keys either.
//!
//! Two modes. `no-home` is the shape above with the network up — an
//! install needs its registry. `airlock` adds `--unshare-net`: loopback
//! only, nothing resolves, nothing phones home.
//!
//! Two things this adds that the reference it was ported from lacked,
//! both from bwrap(1)'s own warnings: `--new-session`, without which
//! the child can `TIOCSTI` keystrokes back into the terminal that
//! launched it — "can lead to out-of-sandbox command execution", in
//! the man page's words — and `--die-with-parent`, so a `kuma run`
//! that dies takes its sandbox with it instead of orphaning one.
//!
//! Inside a container the verb fails by default (`CLONE_NEWNS` needs
//! CAP_SYS_ADMIN there); it is a runtime verb — a machine, or a `kuma
//! vm` guest — never a build step. Sandboxing is per-invocation state,
//! not declaration state, which is why this is a verb and not a
//! compose block.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// The two modes `kuma run` speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Empty home, network up: an install needs its registry.
    NoHome,
    /// Empty home, and the network is loopback only.
    Airlock,
}

/// Whether this working directory would defeat the sandbox. Refusing
/// is the honest answer: the cwd is bound read-write after the tmpfs
/// home, so a cwd that IS the home — or a directory containing it —
/// binds the real home back into a sandbox whose whole point was that
/// the home is empty. Pure so the tests can pin the rule.
fn defeats_empty_home(cwd: &Path, home: &str) -> bool {
    let home = Path::new(home);
    cwd == home || home.starts_with(cwd)
}

/// The bwrap argv for `mode` running `command` from `cwd` with `home`
/// swapped for the empty one. Pure so the tests can pin the shape:
/// the tmpfs home before the cwd bind, the agent sockets unset,
/// `--new-session` and `--die-with-parent` in every mode,
/// `--unshare-net` in airlock alone.
pub(crate) fn bwrap_argv(mode: Mode, cwd: &Path, home: &str, command: &[String]) -> Vec<String> {
    let cwd = cwd.display().to_string();
    let mut args: Vec<String> = [
        "--ro-bind", "/", "/",
        "--dev", "/dev",
        "--proc", "/proc",
        "--tmpfs", "/tmp",
        "--tmpfs", home,
        "--bind", &cwd, &cwd,
        "--setenv", "HOME", home,
        "--unsetenv", "SSH_AUTH_SOCK",
        "--unsetenv", "GPG_AGENT_INFO",
        "--new-session",
        "--die-with-parent",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if mode == Mode::Airlock {
        args.push("--unshare-net".to_string());
    }
    args.push("--".to_string());
    args.extend(command.iter().cloned());
    args
}

/// Run `command` in the sandbox. The child inherits this process's
/// stdio — its output is a stream, and the stream is the product — and
/// its exit code is the verb's, signal deaths included (128+n, the
/// shell's convention). A command that failed is not an error OF the
/// verb; only a sandbox that could not be built is.
pub fn run(mode: Mode, command: &[String]) -> Result<()> {
    if command.is_empty() {
        bail!("run needs a command to sandbox: kuma run no-home -- npm install");
    }
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => h,
        _ => bail!("run needs a HOME to swap for the empty one"),
    };
    let cwd = std::env::current_dir().context("run needs a working directory")?;
    if defeats_empty_home(&cwd, &home) {
        bail!(
            "run refuses to sandbox from {} — the working directory bind would put \
             the real home directory back in, and the empty home is the point. \
             cd into the project and run it there.",
            cwd.display()
        );
    }
    let argv = bwrap_argv(mode, &cwd, &home, command);
    let started = std::time::Instant::now();
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| {
            format!(
                "cannot start {} — every kuma image carries bubblewrap, so this is \
                 neither a machine nor a kuma vm guest",
                argv[0]
            )
        })?;
    crate::host::trace(&argv, started);
    match status.code() {
        Some(code) => std::process::exit(code),
        None => {
            use std::os::unix::process::ExitStatusExt;
            std::process::exit(128 + status.signal().unwrap_or(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv_of(mode: Mode, cwd: &str, home: &str, command: &[&str]) -> Vec<String> {
        bwrap_argv(
            mode,
            Path::new(cwd),
            home,
            &command.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    #[test]
    fn the_sandbox_swaps_the_home_and_keeps_the_cwd() {
        let args = argv_of(Mode::NoHome, "/var/home/u/project", "/var/home/u", &["npm", "install"]);
        let tmpfses: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "--tmpfs")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(tmpfses.len(), 2, "tmp and the home are the two tmpfs mounts: {args:?}");
        assert_eq!(args[tmpfses[0] + 1], "/tmp");
        assert_eq!(args[tmpfses[1] + 1], "/var/home/u");
        let bind_at = args.iter().position(|a| a == "--bind").unwrap();
        // Later mounts win: the cwd bind must come after the tmpfs
        // home, or the project directory would be empty too.
        assert!(tmpfses[1] < bind_at, "{args:?}");
        assert_eq!(&args[bind_at + 1], "/var/home/u/project");
        assert_eq!(&args[bind_at + 2], "/var/home/u/project");
        assert!(
            args.ends_with(["--", "npm", "install"].map(|s| s.to_string()).as_slice()),
            "the command rides after the separator: {args:?}"
        );
    }

    #[test]
    fn the_port_additions_are_in_every_mode_and_the_airlock_is_the_only_difference() {
        let no_home = argv_of(Mode::NoHome, "/tmp/p", "/var/home/u", &["true"]);
        let airlock = argv_of(Mode::Airlock, "/tmp/p", "/var/home/u", &["true"]);
        for args in [&no_home, &airlock] {
            assert!(args.contains(&"--new-session".to_string()), "TIOCSTI: {args:?}");
            assert!(args.contains(&"--die-with-parent".to_string()), "no orphans: {args:?}");
            assert!(args.contains(&"--unsetenv".to_string()), "the agent sockets: {args:?}");
        }
        assert!(!no_home.contains(&"--unshare-net".to_string()));
        // Airlock is no-home with exactly one flag inserted, before the
        // separator — the command's position and everything else is
        // untouched.
        let sep = no_home.iter().position(|a| a == "--").unwrap();
        let mut expected = no_home.clone();
        expected.insert(sep, "--unshare-net".to_string());
        assert_eq!(airlock, expected);
    }

    #[test]
    fn the_cwd_refusal_is_the_home_and_its_ancestors_only() {
        // The home itself, and anything that contains it: both put the
        // real home back in the sandbox.
        assert!(defeats_empty_home(Path::new("/var/home/u"), "/var/home/u"));
        assert!(defeats_empty_home(Path::new("/var"), "/var/home/u"));
        assert!(defeats_empty_home(Path::new("/"), "/var/home/u"));
        // A project under the home is the deal: never refused.
        assert!(!defeats_empty_home(Path::new("/var/home/u/project"), "/var/home/u"));
        // A sibling that merely shares a prefix is fine — starts_with
        // is component-wise, so this is not a string comparison.
        assert!(!defeats_empty_home(Path::new("/var/home/other"), "/var/home/u"));
        assert!(!defeats_empty_home(Path::new("/usr/src"), "/var/home/u"));
    }
}
