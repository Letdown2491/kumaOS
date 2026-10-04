#!/usr/bin/python3
"""Log in at a guest's serial console and run one command.

The live ISO is the one thing kuma ships that cannot be inspected from
outside. There is no disk to loop-mount, no ssh (the live account has no
password, by design, so sshd will not take it), and the interesting
question is not whether files are present but whether a desktop session
actually came up. The serial console is the only channel into it, and
`liveuser` having no password is what makes logging in there possible
rather than a hole: it is installer media whose account cannot survive
onto an installed machine.

Sibling of console-unlock.py, which types a LUKS passphrase at the same
kind of socket. That one answers "does it come up"; this one answers
"what does it say once it has".

Usage: console-session.py SOCKET USER COMMAND [TIMEOUT]

Everything received is echoed to stdout so a failure leaves the guest's
own account of it behind. Exit status is 0 only if the login prompt
appeared, the login was accepted, and the command produced its end
marker.
"""

import re
import socket
import sys
import time

# Printed by the command itself. Waiting for a shell prompt instead would
# mean guessing at PS1, and the prompt string also appears in the echo of
# the command that was just typed.
#
# A serial console echoes what you type, so the marker has to be one the
# typed line cannot contain: the guest is sent a concatenation the shell
# joins (`KUMA_CONSOLE""_DONE`) and the joined form only ever appears in
# real output. Without this the wait matched the echo and every probe
# "succeeded" the instant it was typed.
END = "KUMA_CONSOLE_DONE"
END_TYPED = 'KUMA_CONSOLE""_DONE'


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    path, user, command = sys.argv[1], sys.argv[2], sys.argv[3]
    deadline = float(sys.argv[4]) if len(sys.argv) > 4 else 300.0

    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.connect(path)
    s.settimeout(1.0)
    buf = ""
    closed = False
    end_at = time.time() + deadline

    def pump() -> None:
        nonlocal buf, closed
        try:
            data = s.recv(65536)
            if data:
                buf += data.decode("utf-8", "replace")
            else:
                # A zero-length read is the peer hanging up, which here
                # means qemu exited: the guest is gone and nothing further
                # can arrive.
                closed = True
        except socket.timeout:
            pass

    def wait_for(pattern: str) -> bool:
        while time.time() < end_at:
            if re.search(pattern, buf):
                return True
            # Checked after the search, so data delivered by the same
            # read that saw EOF still counts.
            if closed:
                return False
            pump()
        return False

    if not wait_for(r"login:"):
        sys.stdout.write(buf)
        why = "the guest exited" if closed else f"no login prompt within {deadline:.0f}s"
        print(f"\nconsole-session: {why}", file=sys.stderr)
        return 1
    s.sendall((user + "\n").encode())

    # An empty-password account still goes through login(1), so give it a
    # moment and check we are not being asked for one: a serial console
    # that silently sits at "Password:" would otherwise time out on the
    # command instead of reporting the real cause.
    time.sleep(3)
    pump()
    if re.search(r"[Pp]assword:", buf[-400:]):
        sys.stdout.write(buf)
        print("\nconsole-session: the account asked for a password", file=sys.stderr)
        return 1

    # One budget covers both waits, so a slow login leaves the command
    # less than the full deadline. Reported as what was actually left
    # rather than as the whole number: "did not finish within 420s" after
    # a login that ate 400 of them sends the reader after the wrong thing.
    left = end_at - time.time()
    s.sendall(("\n" + command + "; echo " + END_TYPED + "\n").encode())
    if not wait_for(re.escape(END) + r"\s*\r?\n"):
        sys.stdout.write(buf)
        why = (
            "the guest exited mid-command"
            if closed
            else f"command did not finish in the {left:.0f}s left of a {deadline:.0f}s budget"
        )
        print(f"\nconsole-session: {why}", file=sys.stderr)
        return 1

    sys.stdout.write(buf)
    return 0


if __name__ == "__main__":
    sys.exit(main())
