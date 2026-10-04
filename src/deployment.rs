//! The machine's deployments, read once from `bootc status --format json`.
//!
//! Three walkers parsed that document themselves — the rollback verb,
//! doctor's deployment check, and the report — and reading the same
//! nesting three ways is how a digest got read one level too shallow
//! once: the field read as working, because null is also what a local
//! image without a digest looks like. The slots are parsed here, once,
//! and the walkers read the fields.

/// One bootable system on the machine, in the shape bootc reports it.
pub(crate) struct Slot {
    /// The image reference, e.g. `ghcr.io/letdown2491/kuma:latest`.
    pub image: Option<String>,
    /// The content digest, pinned so a report cannot be satisfied by
    /// the reference alone.
    pub digest: Option<String>,
    /// When the image was built, for doctor's staleness grading.
    pub timestamp: Option<String>,
    /// How the deployment names its image (`containers-storage` when it
    /// follows a local tag), for doctor's tag comparison.
    pub transport: Option<String>,
}

/// booted, and what would boot next: the staged deployment, and the one
/// a rollback would land on. A slot bootc reports as null is absent
/// here, which is the distinction between "no rollback to land on" and
/// "a rollback with no image recorded" that no caller should have to
/// re-derive.
pub(crate) struct Deployments {
    pub booted: Slot,
    pub staged: Option<Slot>,
    pub rollback: Option<Slot>,
}

impl Deployments {
    pub fn from_status_json(json: &serde_json::Value) -> Deployments {
        let slot = |name: &str| {
            json.get("status").and_then(|s| s.get(name)).filter(|v| !v.is_null()).cloned()
        };
        let one = |value: Option<serde_json::Value>| {
            let value = value.unwrap_or_default();
            let field =
                |pointer: &str| value.pointer(pointer).and_then(|v| v.as_str()).map(str::to_string);
            Slot {
                image: field("/image/image/image"),
                digest: field("/image/imageDigest"),
                timestamp: field("/image/timestamp"),
                transport: field("/image/image/transport"),
            }
        };
        Deployments {
            booted: one(slot("booted")),
            staged: slot("staged").map(|v| one(Some(v))),
            rollback: slot("rollback").map(|v| one(Some(v))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> serde_json::Value {
        serde_json::json!({"status": {
            "booted": {"image": {
                "image": {"image": "ghcr.io/letdown2491/kuma:latest",
                          "transport": "containers-storage"},
                "imageDigest": "sha256:0123456789abcdef0123456789abcdef",
                "timestamp": "2026-08-01T00:00:00Z",
            }},
            "staged": null,
            "rollback": {"image": {
                "image": {"image": "ghcr.io/letdown2491/kuma:previous"},
                "imageDigest": "sha256:fedcba9876543210fedcba9876543210",
            }},
        }})
    }

    /// Every field of every slot, at the depth bootc writes it. The
    /// digest sits beside the reference inside the image object, one
    /// level deeper than a slot's own fields — the nesting that was
    /// read too shallow once, and read as working because null is also
    /// what a digest-less local image looks like.
    #[test]
    fn slots_read_the_fields_where_bootc_writes_them() {
        let deps = Deployments::from_status_json(&status());
        assert_eq!(deps.booted.image.as_deref(), Some("ghcr.io/letdown2491/kuma:latest"));
        assert_eq!(deps.booted.digest.as_deref(), Some("sha256:0123456789abcdef0123456789abcdef"));
        assert_eq!(deps.booted.timestamp.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(deps.booted.transport.as_deref(), Some("containers-storage"));
        assert_eq!(
            deps.rollback.as_ref().and_then(|r| r.image.as_deref()),
            Some("ghcr.io/letdown2491/kuma:previous"),
        );
        // A slot that only names an image reports no digest rather than
        // borrowing the one beside it.
        assert!(deps.rollback.as_ref().is_some_and(|r| r.digest.is_some()));
    }

    /// A null slot is an absent one: the machine has nothing staged and
    /// no rollback to land on, which is a different answer from a slot
    /// whose fields are merely empty.
    #[test]
    fn a_null_slot_is_absent() {
        let deps = Deployments::from_status_json(&status());
        assert!(deps.staged.is_none());
    }

    /// Missing sections read as absent too — a non-bootc machine's
    /// document has no `status` at all, and the booted slot's fields
    /// must degrade to None rather than panic or invent.
    #[test]
    fn an_empty_document_reads_as_an_empty_machine() {
        let deps = Deployments::from_status_json(&serde_json::json!({}));
        assert!(deps.booted.image.is_none());
        assert!(deps.booted.digest.is_none());
        assert!(deps.staged.is_none());
        assert!(deps.rollback.is_none());
    }
}
