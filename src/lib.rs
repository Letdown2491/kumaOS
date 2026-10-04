//! The nostr layer (44.4.0): code shared between the bins that speak it.
//!
//! `kuma` the system tool does not link any of this; the layer's daemon
//! (`kuma-nostrd`) and CLI (`kuma-nostr`) do. It lives as a library so
//! those bins hold one compiled copy of the vault and its key handling,
//! and so the offline test suite for the pure parts runs here, where the
//! daemons that consume it cannot quietly diverge from what was proven.
//!
//! What is here is deliberately narrow: the vault (a gate-style lock over
//! the Secret Service) and the key import/decode surface. The transport,
//! the policy engine and the socket protocol are daemon concerns and will
//! live beside the daemon when they arrive; what forced their hand into a
//! library was the vault, which both binaries must agree on down to the
//! byte format of what the keyring holds.

pub mod nostr;
pub mod relay;
