# For agents

This is the interface for driving kuma from a program rather than by hand.
Every command that reports or changes something speaks `--json`, and every
response names the legal next commands, so a caller can follow the output
instead of encoding kuma's rules. The exceptions are the verbs that produce a
file or a stream rather than an answer, and they are listed below.

The loop is: probe, execute one of the actions it named, probe again.

**A check that could not be made grades `warn`, not `fail`.** Several of what
`doctor` grades need root, and an agent has no terminal for `sudo` to prompt
at. A declined password is not a broken machine, so none of those report one.

Most of them name what could not be asked. Not all: a check whose privileged
read fails may instead report the state it inferred without it, and the
deployment check is one, so an agent that has not run `doctor` through `sudo`
can be told a newer image is waiting when the answer is really that nobody
could look. **Run the whole verb through `sudo` when you need a definite
answer**, and treat a `warn` from an unprivileged run as "unknown" rather than
as a finding.

**`KUMA_TRACE=1` prints one line per subprocess on stderr**, as elapsed time
and a canonical command name, leaving `--json` on stdout untouched. It is a
diagnostic rather than an interface: the names are stable enough to diff
between runs, and nothing promises they will not gain detail.

- **Probe.** `kuma --json` is the root resource: state, facts, and `actions`
  as `{rel, cmd, why}`. Execute an action's `cmd` verbatim, then re-probe.
  `doctor --json` and `diff --json` carry findings with their fixes in the
  same shape, and every document carries `ok` first: false on doctor means
  a check failed, the counts sit in `summary`, and the whole answer is one
  document whatever the verdict. The `facts` keys are always `config`,
  `image` and `machine`, whatever the state says.
- **A live session says so.** Booted from installer media, the state is
  `live` and the facts describe media rather than a machine. Nothing is
  converging there and nothing persists, so most of what `doctor` grades
  does not apply and it reports one line instead of failing checks that
  were never going to pass. Its one action is `kuma install`, bare: the
  disk is not knowable from there, so the affordance is the form that
  lists what it found and asks.
- **A converging machine says so.** A first boot spends minutes installing
  declared apps, and for that whole window the machine genuinely does not
  match its declaration. The state is `converging`, not `drifted`, and no
  `sync` action is offered because a sync is what is running. Re-probe
  rather than acting: the machine is already doing the thing an agent
  would otherwise tell it to do.
- **Ask before doing.** `check --json` validates a declaration,
  `update --check --json` reports whether the base moved (or, for a composed
  base, every package that has and its advisory severity), `diff --json` reports
  drift, `snapshot --json` lists what this machine has kept, and
  `backup --json` reports the offsite repository, whether this machine has
  the credential the declaration names, and when a copy last completed.
  `hibernate --json` reports what a swapfile would cost and where it would go.
  All six change nothing, and `snapshot --restore --json`,
  `backup --restore --json` and `hibernate --off --json` stay dry runs naming
  what they would overwrite or remove until `--yes`.

  `backup --json` answers without touching the network, deliberately, so an
  agent polling machine health never blocks on a repository being reachable.
  `backup --list` is the one that asks the far end.
- **What does not speak JSON, and why.** `init`, `generate`, `vm`, `iso`,
  `edit`, `schema`, `passwd` and `completions` each produce a file, a stream,
  or an editor rather than a report, so there is no document for them to emit
  and no next command for them to name. `edit --print` answers the one
  question an agent would ask of it: which declaration this machine resolves. `schema` already emits JSON, of a
  different kind: the declaration's schema rather than a response.
- **A Fedora release change is a separate field, not a big diff.** `update`
  and `update --check` carry `fedora_release` with `current`, `changed`,
  `from` and `to`. Read that rather than inferring a distro upgrade from the
  size of the package list, and treat `changed: true` as needing a human
  even where you would otherwise stage automatically. Only `update` can
  report a move. `update --check` neither composes nor pulls, so it has
  nothing to compare against and always answers `changed: false` with
  `from` and `to` null: read `current` from a check and `changed` from an
  update. `current` is null when the release could not be read, which is
  not the same as a release that did not change, and is also what a
  machine with no base image in local storage reports, since a check does
  not download one to answer.
- **A sync that cannot help says so.** `sync --json` carries
  `baked_declaration_behind`. Convergers read the declaration the image
  baked, so when that is behind the file in hand, converging applies the
  image's lists rather than the edit, however many times it runs. True there
  means the next action is `build`, not another `sync`.
- **Reporting a broken machine.** `doctor --report` is `doctor --json` plus
  `kuma.version`, a `machine` object (`pretty_name`, `version_id`,
  `booted_image`, `booted_digest`, `staged`, `rollback`, `live_media`), and
  the `declaration` the machine was built from. `checks` and `summary` keep
  the same shape and place, so anything already reading `--json` reads a
  report unchanged. `user.password_hash` is removed before it prints, and a
  declaration kuma cannot parse arrives as `declaration.omitted` rather than
  as raw text. A check whose detail quotes a failed unit's own output carries
  it masked of anything shaped like a crypt hash, so a pasted report cannot
  leak one by way of a journal line. That is the payload to attach to a bug
  report.
- **Write.** `kuma schema` prints the JSON Schema for `kuma.toml`, generated
  from the same types that parse it, so it cannot drift from reality.
- **Mutate.** `build`, `switch`, `update`, `rollback`, `sync`, `add`,
  `capture`, `remove`, `clean`, `hibernate`, and `install` accept `--json`
  and emit exactly one document on stdout: `{"ok": true, …}` with result
  fields and next `actions`, or `{"ok": false, "error": …}` with a non-zero
  exit. A verb that ran as a preview because its `--yes` was missing marks
  its document `"dry_run": true`, so a caller tells a preview from an
  effect in the document alone. Progress and subprocess output move to
  stderr. The read verbs answer with the same failure document when they
  fail: no `--json` run of kuma ends with an unparseable stdout.
- **Nothing changes what's running without a reboot.** `switch`, `update` and
  `rollback` gate on `--yes`, and even then only stage a deployment.

`kuma install` is the one exception, and worth understanding before driving
it. It writes a disk immediately and cannot be undone: no staged deployment
to discard, no rollback slot. Three things make it drivable anyway:

- **It dry-runs by default.** Without `--yes` it reports the disk, the image,
  whether that image is already local, the partition `layout` it will write,
  whether the root will be `encrypted`, and what it will ask a person for
  (`asks`). It also refuses a disk with anything mounted on it.
- **It is the only verb that prompts.** Give it `--user`, `--hostname` and
  `--disk` and the password is the one remaining answer, read from stdin
  rather than a flag, so it never reaches `ps` or a shell history.
- **Encryption is opt-in from a program.** `--encrypt` makes stdin two lines,
  the disk passphrase first and the account password second, in the order a
  person is asked. Without the flag an install driven this way is never
  encrypted, because the question is only put to a terminal.
- **Partition sizes are answerable from a program.** `--esp` and `--boot`
  name the two sizes the layout will carry, and like `--encrypt` they are the
  whole of the answer a non-terminal can give: an install driven this way
  without them takes the defaults, 600M and 2G. The dry run's `layout` shows
  what was resolved, and `asks` names each size that was not.

Without `--config`, kuma reads `./kuma.toml`, falling back to
`~/.config/kuma/kuma.toml`. Neither is ever created implicitly. With no
working copy at all, read-only commands fall back to the machine's baked
declaration, so an ISO-installed machine can `kuma update --yes` without
ever creating a file; editing is what requires one.


## The nostr layer's surface

The layer ships its own CLI — `kuma-nostr` — and daemon — `kuma-nostrd` —
speaking the same house shape: `ok` first, `error` on a refusal, `--json`
for the document form, and the socket answers the same documents
newline-delimited at `$XDG_RUNTIME_DIR/kuma-nostr.sock`.

An agent's honest path is the CLI, and the read verbs are free:
`kuma-nostr status --json` (the vault's existence, gate, identity, the
pairing URI, the inactivity switch), `prompts --json` (the pending asks,
each with `id`, `app`, `method`, `summary` and, for a signature, the exact
unsigned event as `detail`), and `apps --json` (the pairings: level,
name, claimed perms, the tombstone). `approve --json --id <id>` and
`deny --json --id <id>` answer an ask; `--remember 1` grants the method
an hour. That is the whole decision surface an agent gets — the panel and
the CLI are equals in front of the same engine, and neither can widen an
app past its level.

The mutating pair verbs do what their names say and say so before they do
it: `bunker --json` mints a one-time pairing URI (`uri` in the document —
one app, one use, burned by the connect that presents it), `connect
--json --uri <nostrconnect-uri>` pairs a client's own invite, `revoke`
tombstones (the record stays, refused), `unrevoke` clears the tombstone,
`delete` removes the record outright (a fresh URI pairs again), `label`
names an app (the person's word, outranking the client's metadata claim),
and `log` reads the activity — the last 500 asks, answers and pairings,
persisted across restarts.
`rotate` invalidates every outstanding URI at once, `lock` and `unlock`
are the gate, and `destroy` is a dry run until `--yes`.

Two things an agent must not do are structural: the socket's peer check
drops anything not running as the daemon's uid, and no verb reads a
secret from a flag — keys arrive on stdin or through the keyring, so
they cannot reach a `ps` line or a shell history.
