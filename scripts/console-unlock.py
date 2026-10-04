#!/usr/bin/python3
"""Type a LUKS passphrase at a guest's serial console.

An encrypted root stops in the initramfs and asks for a passphrase, which
is why every automated check kuma had of an encrypted install read the
disk through a loop device instead of booting it: the header, the kargs
and the account file can all be inspected offline, but "does it come up
when you type the passphrase" cannot. This is the piece that lets a test
answer the only question a person actually has.

It does what a person does. It does not add a keyfile, a TPM enrolment or
a second keyslot, because each of those would test a machine nobody
installs.

Usage: console-unlock.py SOCKET PASSPHRASE [TIMEOUT]

The socket is qemu's serial chardev in server mode. Output is echoed to
stdout so a failure leaves the guest's own account of it behind, and the
exit status says whether the passphrase was ever asked for.
"""

import socket
import sys
import time

# systemd-cryptsetup asks "Please enter passphrase for disk ...". Matched
# loosely because the wording has changed before and the cost of matching
# too narrowly is a test that hangs for five minutes and then says
# nothing useful.
PROMPTS = (b"passphrase", b"password for")
# The far side of a successful unlock: the machine reaches a getty. Also
# the signal to stop reading, so a booted machine does not hold the job
# open until the timeout.
DONE = (b"login:", b"systemd-cryptsetup: set up successfully")
# Bounded because a wrong passphrase is re-prompted forever, and a test
# that retypes it forever is a hang rather than a failure.
MAX_ATTEMPTS = 3
# How long to ignore prompts after typing one. A console arrives in
# whatever chunks the socket gives, and the real prompt ("Please enter
# passphrase for disk root (luks-...)") split across two reads, so the
# second half matched again and the passphrase was typed twice into a
# machine that had already accepted it. Deriving a key takes seconds, and
# a genuine re-prompt after a wrong passphrase comes later than this.
RETYPE_COOLDOWN = 15.0


def main() -> int:
    sock_path, passphrase = sys.argv[1], sys.argv[2]
    timeout = float(sys.argv[3]) if len(sys.argv) > 3 else 420.0
    deadline = time.time() + timeout

    conn = None
    while conn is None and time.time() < deadline:
        try:
            conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            conn.connect(sock_path)
        except OSError:
            conn = None
            time.sleep(0.5)
    if conn is None:
        print("console-unlock: never connected to", sock_path, file=sys.stderr)
        return 2

    conn.settimeout(5)
    # Only the tail is matched, so a prompt cannot be found again in
    # scrollback and answered twice.
    tail = b""
    attempts = 0
    typed_at = 0.0
    while time.time() < deadline:
        try:
            data = conn.recv(4096)
        except TimeoutError:
            continue
        except OSError:
            break
        if not data:
            break
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()
        tail = (tail + data)[-512:]
        lowered = tail.lower()

        if any(marker in lowered for marker in DONE):
            print("\nconsole-unlock: guest is past the unlock", file=sys.stderr)
            return 0 if attempts else 3

        if (
            attempts < MAX_ATTEMPTS
            and time.time() - typed_at > RETYPE_COOLDOWN
            and any(p in lowered for p in PROMPTS)
        ):
            conn.sendall(passphrase.encode() + b"\n")
            attempts += 1
            typed_at = time.time()
            print(f"\nconsole-unlock: typed the passphrase ({attempts})", file=sys.stderr)
            tail = b""

    if attempts == 0:
        print("console-unlock: never saw a passphrase prompt", file=sys.stderr)
        return 4
    # Asked and answered, but no getty before the timeout. The boot
    # assertions decide what that means; this only reports what it saw.
    print("console-unlock: typed the passphrase, no getty yet", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
