use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set when the verb's stdout is a JSON document: everything else —
/// progress notes, subprocess chatter — must land on stderr instead, so
/// an agent can parse stdout without sifting.
static JSON_OUTPUT: AtomicBool = AtomicBool::new(false);

pub fn set_json_output() {
    JSON_OUTPUT.store(true, Ordering::Relaxed);
}

/// A progress note: stdout for humans, stderr when stdout carries JSON.
pub fn note(msg: &str) {
    if JSON_OUTPUT.load(Ordering::Relaxed) {
        eprintln!("{msg}");
    } else {
        println!("{msg}");
    }
}

/// One line per subprocess, behind `KUMA_TRACE`.
///
/// Every measurement in the 2026-08-21 performance review was taken by
/// hand, and the ranking that came out of it did not survive contact
/// with a stopwatch: the item ranked first cost 1.1s and the one
/// mentioned in passing cost 4.6s on every build. That is not a failure
/// of judgement, it is what happens when the numbers have to be produced
/// by a person who already has a hypothesis.
///
/// So the numbers stop being something anybody has to produce. Set
/// `KUMA_TRACE=1` and every command kuma runs prints what it was and how
/// long it took, on stderr, where it does not disturb `--json` on stdout.
///
/// CANONICAL rather than the full argv: a digest, a tempdir and an image
/// tag change on every run, and a ledger nobody can diff is a ledger
/// nobody reads. `podman image inspect --format {{.Id}} localhost/kuma:latest`
/// prints as `podman image inspect`, which is stable, and two identical
/// lines in one verb's ledger is itself a finding.
fn canonical<S: AsRef<str>>(args: &[S]) -> String {
    let words: Vec<&str> = args.iter().map(|a| a.as_ref()).collect();
    let mut out: Vec<&str> = words
        .iter()
        .copied()
        .take_while(|word| !word.starts_with('-') && !word.contains('/') && !word.contains('='))
        .take(3)
        .collect();
    // `sh -c <script>` would otherwise print as `sh`, and the first two
    // lines of doctor's own ledger were two indistinguishable `sudo sh`
    // entries at 2.2s and 1.9s. An instrument that cannot tell its most
    // expensive entries apart is not one. The script's first word is
    // what it actually ran.
    if let Some(index) = words.iter().position(|word| *word == "-c") {
        if let Some(script) = words.get(index + 1) {
            if let Some(first) = script.split_whitespace().next() {
                out.push(first);
            }
        }
    }
    out.join(" ")
}

fn trace<S: AsRef<str>>(args: &[S], started: std::time::Instant) {
    if std::env::var_os("KUMA_TRACE").is_none() {
        return;
    }
    eprintln!("[kuma-trace] {:7.3}s  {}", started.elapsed().as_secs_f64(), canonical(args));
}

/// Run a command on the host, escaping the container if kuma itself is
/// running inside one (e.g. a distrobox dev environment).
pub fn run_host<S: AsRef<str>>(args: &[S]) -> Result<()> {
    run_piped(args, None)
}

/// Run a command on the host, handing it `input` on standard input.
///
/// For the one thing kuma has that must not appear in argv: a disk
/// passphrase. `ps` shows any process's arguments to any user, so a
/// secret passed as one is readable by everybody on the machine for as
/// long as the command runs, and a file would leave it on a disk.
///
/// `sudo` still gets its own prompt: it reads a password from the
/// terminal rather than from stdin unless it is asked to do otherwise,
/// so a piped stdin does not swallow the one the person has to answer.
pub fn run_host_stdin<S: AsRef<str>>(args: &[S], input: &str) -> Result<()> {
    run_piped(args, Some(input))
}

/// The body both share, because they differ in one thing and the rest of
/// it (finding the host, moving stdout out of the way of JSON, naming
/// the command that failed) was worth writing once.
fn run_piped<S: AsRef<str>>(args: &[S], input: Option<&str>) -> Result<()> {
    use std::io::Write;
    let started = std::time::Instant::now();
    let mut cmd = host_command(args)?;
    if input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    if JSON_OUTPUT.load(Ordering::Relaxed) {
        // The child's stream (podman build output, bootc progress) stays
        // visible — just not where the JSON goes.
        use std::os::fd::AsFd;
        let stderr = std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .context("cannot redirect subprocess output to stderr")?;
        cmd.stdout(std::process::Stdio::from(stderr));
    }
    let mut child = cmd.spawn().with_context(|| format!("failed to run {}", args[0].as_ref()))?;
    // A write that fails is not the failure worth reporting. When sudo
    // refuses a password it exits before reading, and this write then
    // returns EPIPE: reporting that would replace "sudo: 3 incorrect
    // password attempts" with "cannot write to the child process", which
    // sends somebody looking at the wrong thing entirely. So the child's
    // own status is asked first, and the write is only news if the
    // command otherwise succeeded.
    let wrote = match (input, child.stdin.take()) {
        (Some(input), Some(mut stdin)) => stdin.write_all(input.as_bytes()),
        (Some(_), None) => Ok(()),
        (None, _) => Ok(()),
    };
    let status = child.wait().with_context(|| format!("failed to run {}", args[0].as_ref()))?;
    trace(args, started);
    if !status.success() {
        let shown: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
        bail!("{} exited with {status}", shown.join(" "));
    }
    wrote.context("cannot write to the child process")?;
    Ok(())
}

/// Run a host command and capture its trimmed stdout; Err on failure.
pub fn host_output<S: AsRef<str>>(args: &[S]) -> Result<String> {
    let started = std::time::Instant::now();
    let out = host_command(args)?
        // Captured rather than discarded, so a failure that reaches a
        // human carries the tool's own words. podman saying "Treating
        // single images as manifest lists is not implemented" is worth
        // rather more than "exited with exit status: 125". Probes that
        // expect to fail drop the Err anyway, so nothing gets noisier.
        .stderr(std::process::Stdio::piped())
        .output()
        .with_context(|| format!("failed to run {}", args[0].as_ref()))?;
    trace(args, started);
    if !out.status.success() {
        let shown: Vec<&str> = args.iter().map(|s| s.as_ref()).collect();
        bail!("{} exited with {}{}", shown.join(" "), out.status, reason(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The tail of stderr, where tools put the reason. Bounded in both
/// directions: the last two lines, each clipped, because some of them
/// answer with a kilobyte of JSON and an error nobody can read is worth
/// no more than the exit code it replaced.
fn reason(stderr: &[u8]) -> String {
    const WIDTH: usize = 200;
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail: Vec<String> = lines[lines.len().saturating_sub(2)..]
        .iter()
        // Clipped from the middle, not the end: tools put the reason at
        // either end and sometimes both. podman answers a bad manifest
        // request with a kilobyte of echoed JSON and then, last of all,
        // the sentence that explains it.
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            if chars.len() <= WIDTH {
                return line.to_string();
            }
            let half = WIDTH / 2;
            let head: String = chars[..half].iter().collect();
            let tail: String = chars[chars.len() - half..].iter().collect();
            format!("{head} ... {tail}")
        })
        .collect();
    if tail.is_empty() {
        String::new()
    } else {
        format!(": {}", tail.join("; "))
    }
}

/// Capture stdout even when the command exits non-zero — for tools like
/// `systemctl is-enabled` that report state through stdout AND exit status.
/// Err only when the command cannot run at all.
pub fn host_output_any<S: AsRef<str>>(args: &[S]) -> Result<String> {
    let out = host_command(args)?
        .stderr(std::process::Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", args[0].as_ref()))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A private working directory whose contents are handed to a
/// root-run script.
///
/// Every privileged verb stages the same way: write a script and the
/// files it reads into a fresh tempdir, then run the script as root.
/// The discipline that kept going wrong lives here, once.
///
/// The directory is 0700 for whoever created it, and root reads through
/// that without help — the 2026-08-31 audit confirmed it against the
/// code's own record: the widen this directory used to perform before a
/// run (`chmod -R a+rX` over everything, credentials pruned) made the
/// staged scripts and build context world-readable for the length of a
/// run, and nothing that reads them needed it. The widen is gone; what
/// protects a credential is the mode it is created with.
///
/// A credential is written 0600 from the moment it exists — created
/// with the mode, and forced to it even over a file that is already
/// there — and a plain file can never land on a credential's name,
/// because a plain file's mode would silently widen the secret that
/// name holds.
///
/// OPEN QUESTION, recorded 2026-08-31 and not yet measured: when kuma
/// itself runs inside a container, `host_command` escapes to the host
/// but this directory is created in the container's /tmp, so a
/// host-side `sudo bash /tmp/…/enable` looks for the script in the
/// host's /tmp. Either distrobox shares /tmp and this is fine, or the
/// staging path breaks under distrobox and the escape seam needs to
/// hand the host a directory the host can see. Needs one distrobox
/// run to answer; correctness of the seam, not security.
pub struct ForRoot {
    /// Held rather than borrowed so the directory lives exactly as
    /// long as the staging does and is removed when the verb ends.
    dir: tempfile::TempDir,
    /// The names already staged as credentials. `file` refuses them:
    /// a plain file's mode would silently widen the secret such a
    /// name holds.
    credentials: Vec<String>,
}

/// Stage a working directory for a root-run script.
pub fn for_root() -> Result<ForRoot> {
    Ok(ForRoot {
        dir: tempfile::tempdir().context("cannot create a working directory")?,
        credentials: Vec::new(),
    })
}

impl ForRoot {
    /// The directory itself, for the one argument a staged script
    /// takes that is neither a disk nor a size: its own context.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write a file the script will read, and return where it landed.
    /// `name` is a basename; the directory is the adapter's to place.
    pub fn file(&self, name: &str, contents: &str) -> Result<PathBuf> {
        if self.credentials.iter().any(|staged| staged == name) {
            bail!("cannot stage {name} as a plain file: it is already a credential, and this would widen it");
        }
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents).with_context(|| format!("cannot stage {name}"))?;
        Ok(path)
    }

    /// Write a credential: 0600 from the moment it exists, and kept
    /// there even when a file of the same name already exists, whose
    /// mode would otherwise survive the truncate. The mode is the whole
    /// of the protection; nothing that reads these needs anything more
    /// than root already has.
    pub fn credential(&mut self, name: &str, contents: &str) -> Result<PathBuf> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let path = self.dir.path().join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("cannot stage {name}"))?;
        file.write_all(contents.as_bytes()).with_context(|| format!("cannot stage {name}"))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("cannot keep {name} private"))?;
        self.credentials.push(name.to_string());
        Ok(path)
    }

    /// Run a staged script as root, discarding its output.
    pub fn run<S: AsRef<str>>(&self, script: &Path, args: &[S]) -> Result<()> {
        run_host(&sudo_bash(script, args)?)
    }

    /// Run a staged script as root, feeding it `input` on stdin: the
    /// one channel for what must not appear in argv, which is why a
    /// disk passphrase travels this way.
    pub fn run_stdin<S: AsRef<str>>(&self, script: &Path, args: &[S], input: &str) -> Result<()> {
        run_host_stdin(&sudo_bash(script, args)?, input)
    }

    /// Run a staged script as root and capture its stdout, for the
    /// scripts that answer with a fact rather than an exit code.
    pub fn output<S: AsRef<str>>(&self, script: &Path, args: &[S]) -> Result<String> {
        host_output(&sudo_bash(script, args)?)
    }
}

/// `sudo bash <script> <args…>`, gathered because that is the one
/// shape a staged script is run in and no other.
fn sudo_bash<S: AsRef<str>>(script: &Path, args: &[S]) -> Result<Vec<String>> {
    let mut argv = vec![
        "sudo".to_string(),
        "bash".to_string(),
        script.to_str().context("non-UTF-8 script path")?.to_string(),
    ];
    argv.extend(args.iter().map(|a| a.as_ref().to_string()));
    Ok(argv)
}

fn host_command<S: AsRef<str>>(args: &[S]) -> Result<Command> {
    let in_container =
        Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists();
    let mut full: Vec<&str> = Vec::new();
    if in_container {
        full.extend(["flatpak-spawn", "--host"]);
    }
    full.extend(args.iter().map(|s| s.as_ref()));

    let mut cmd = Command::new(full[0]);
    cmd.args(&full[1..]);
    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The staging half, testable without root because only the run
    /// half needs it: a plain file round-trips, and a credential is
    /// 0600 from the moment it exists rather than a write followed by
    /// a narrowing that a crash in between would undo.
    #[test]
    fn files_stage_and_credentials_start_private() {
        use std::os::unix::fs::PermissionsExt;
        let mut root = for_root().expect("a tempdir is all this needs");
        let plain = root.file("Containerfile", "FROM scratch\n").expect("staged");
        assert_eq!(std::fs::read_to_string(&plain).unwrap(), "FROM scratch\n");
        let secret = root.credential("kuma-user", "KUMA_USER='root'\n").expect("staged");
        let mode = std::fs::metadata(&secret).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(root.path().join("kuma-user").exists());
    }

    /// A credential's protection is its mode, and a plain file's write
    /// is 0644: staging one over a credential's name would widen the
    /// secret that name holds. The adapter refuses rather than allows
    /// the shape to exist.
    #[test]
    fn a_plain_file_cannot_land_on_a_credential() {
        let mut root = for_root().expect("a tempdir is all this needs");
        root.credential("kuma-user", "KUMA_USER='root'\n").expect("staged");
        let attempted = root.file("kuma-user", "not a secret\n");
        assert!(attempted.is_err(), "file() must refuse a credential's name");
        let secret = root.path().join("kuma-user");
        assert_eq!(
            std::fs::read_to_string(&secret).unwrap(),
            "KUMA_USER='root'\n",
            "the refusal happened before the write"
        );
    }

    /// The mode applies at create, so a credential staged over a file
    /// that is already there would inherit its 0644. The adapter forces
    /// the mode after the write instead of trusting it.
    #[test]
    fn a_credential_stays_private_over_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let mut root = for_root().expect("a tempdir is all this needs");
        let path = root.file("kuma-user", "placeholder\n").expect("staged wide");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "the fixture is the hazard: {mode:o}");
        root.credential("kuma-user", "KUMA_USER='root'\n").expect("restaged as a credential");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the credential inherited the wider mode: {mode:o}");
    }
}
