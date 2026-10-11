# kumaOS

One system built and kept from one file: a declaration describes the
machine, an image is built from it, and the running machine is kept
matching it. This glossary fixes the words the project argues with;
docs/glossary.md serves readers new to the underlying Linux ideas.

## Language

**Declaration**:
The `kuma.toml` stating the whole machine. The one file a person
edits; everything else in the system derives from it.
_Avoid_: config, spec, manifest

**Base**:
The floor image kuma composes from Fedora's repositories when the
declaration names no `system.base`: Fedora's minimal bootc core plus
what a real machine needs, nothing removed after the fact.
_Avoid_: base image (say "declared base" for the image `system.base`
names; the unqualified "base" is always the composed one)

**Content tag**:
The composed base's name: a hash of the base manifest that produced
it. A build can name its base before any compose has run, and a
changed manifest cannot reuse a stale one.
_Avoid_: cache key, version tag

**Operating tools**:
The package class the base carries for operating the machine: ping,
ip, the ssh client, the pager, curl. Machine operation, not work;
editors and toolchains are work and belong to the declaration's
package lists.
_Avoid_: default packages, utils, CLI tools

**Floor**:
The set of security settings baked into every image that no
declaration can turn off.
_Avoid_: hardening config, security baseline

**Deployment**:
One bootable instance of the image on disk. The machine boots one of
its slots; going back is a reboot.
_Avoid_: install, snapshot (a snapshot is home's copy, a different
thing)

**Drift**:
The running machine differing from the declaration.
_Avoid_: divergence, desync

**Convergence**:
Bringing the running machine back to the declaration, at boot and on
the daily timer, so an update lands completely or not at all.
_Avoid_: sync (that is the verb, not the concept), reconcile

**Golden**:
A checked-in file pinning generated output byte for byte. A moving
golden is the review, not noise.
_Avoid_: snapshot test, fixture

**Seam**:
The desktop-entry layer between the shell and the declaration's
verbs: entries the shell searches, every exec through kuma-launch.
_Avoid_: launcher entries, shortcuts
