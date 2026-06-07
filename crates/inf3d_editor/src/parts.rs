//! Object typing and the body-part rig hierarchy.
//!
//! This is the groundwork the Phase-2 Animator builds on. A model is an
//! [`ObjectType`] (a posable `Character` or a single-piece `Item`). A character
//! owns a tree of [`Part`]s — `Head`, `Torso`, limbs — each with a **name**, a
//! **pivot** (the joint it rotates about, in model-local world units), and a
//! **parent** ([`PartId`]). Animation in Phase 2 walks this tree, rotating each
//! part about its pivot and inheriting its parent's transform; this phase only
//! has to *define* the tree and tag every sub-voxel with the part that owns it
//! (see [`crate::volume`]).
//!
//! The part *set* is **data-driven**: [`default_character_parts`] seeds the
//! standard humanoid rig, but the UI can rename, re-pivot, re-parent, add, and
//! delete parts. Nothing hard-codes "there are exactly six parts" outside that
//! one seed function.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// What kind of thing the model represents. Stored in the `.ron` sidecar so the
/// game importer knows how to instantiate the model.
///
/// Both variants own a full [`PartTree`] and support the same multi-part rig
/// editing — the type is a *semantic* tag (how the game treats the model), not a
/// capability gate. A `Character` seeds the standard humanoid rig; an `Item`
/// seeds a single `Body` part, but an item may add more named parts (e.g. a
/// torch flame, a flapping banner, a hinged lid) so it can be animated piecewise.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ObjectType {
    /// A posable, multi-part actor (player, mob, NPC). Seeded with a humanoid rig.
    Character,
    /// An object (sword, torch, pickup). Seeded with a single `Body` part, but
    /// may have multiple named, hierarchical, animatable parts of its own.
    Item,
}

impl ObjectType {
    /// Human-readable label for the UI / serialization round-trips.
    pub const fn label(self) -> &'static str {
        match self {
            ObjectType::Character => "Character",
            ObjectType::Item => "Item",
        }
    }
}

/// Stable identifier for a [`Part`] within one model's [`PartTree`].
///
/// IDs are dense indices into [`PartTree::parts`] and are *not* reused after a
/// delete (the slot is tombstoned) so a sub-voxel's stored `part` never silently
/// re-points at a different part. Persisted verbatim in the `.ron` sidecar.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PartId(pub u32);

/// One rig part: a named, pivoted node in the hierarchy that owns a set of
/// sub-voxels (the membership lives on the voxels in [`crate::volume`], not
/// here, so a part is cheap to move/re-parent without touching geometry).
///
/// This is the runtime form (carries a `Vec3` pivot). The *persisted* form lives
/// in [`crate::io::rig::RigPart`] with a `[f32; 3]` pivot, so the on-disk format
/// never depends on bevy_math's optional `serde` feature (which the workspace
/// does not enable — see `inf3d_menu::save`).
#[derive(Clone, Debug)]
pub struct Part {
    /// This part's stable id.
    pub id: PartId,
    /// Display / export name (`"Head"`, `"RightArm"`, …). Editable.
    pub name: String,
    /// Parent in the hierarchy, or `None` for the root. The Animator composes a
    /// part's world transform by walking up these links.
    pub parent: Option<PartId>,
    /// Joint pivot in **model-local world units** — the space where one
    /// reference block is one unit (a sub-voxel cell `c` sits at
    /// `c * VoxelModel::sub_voxel_size()`). This is the point the part rotates
    /// about when animated. Defaults to the part's nominal joint location; the UI
    /// lets the user nudge it.
    pub pivot: Vec3,
}

/// The full part hierarchy for one model. A flat arena of [`Part`]s plus the
/// designated root, addressed by [`PartId`]. Slots are tombstoned (`None`) on
/// delete to keep ids stable.
#[derive(Clone, Debug)]
pub struct PartTree {
    /// Arena of parts; a `None` slot is a deleted (tombstoned) id.
    parts: Vec<Option<Part>>,
    /// The root part id (parent of the rest). Always points at a live slot.
    root: PartId,
}

impl PartTree {
    /// Build a tree from an explicit list of parts, taking the first as root.
    /// Used by the seed functions; callers pass parts whose `parent` links are
    /// already consistent.
    fn from_parts(parts: Vec<Part>) -> Self {
        let root = parts.first().map(|p| p.id).unwrap_or(PartId(0));
        Self {
            parts: parts.into_iter().map(Some).collect(),
            root,
        }
    }

    /// Rebuild a tree from a pre-validated id-indexed arena (the load path).
    /// `slots[i]` is the part with id `i` or `None` for a tombstoned id; `root`
    /// must reference a live slot (the caller validates this).
    pub fn from_arena(slots: Vec<Option<Part>>, root: PartId) -> Self {
        Self { parts: slots, root }
    }

    /// The root part id.
    pub fn root(&self) -> PartId {
        self.root
    }

    /// Borrow a part by id, if the slot is live.
    pub fn get(&self, id: PartId) -> Option<&Part> {
        self.parts.get(id.0 as usize).and_then(|p| p.as_ref())
    }

    /// Mutably borrow a part by id, if the slot is live.
    pub fn get_mut(&mut self, id: PartId) -> Option<&mut Part> {
        self.parts.get_mut(id.0 as usize).and_then(|p| p.as_mut())
    }

    /// Iterate all live parts in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Part> + '_ {
        self.parts.iter().filter_map(|p| p.as_ref())
    }

    /// Number of live parts. A valid tree always keeps at least its root, so
    /// there is deliberately no `is_empty`.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.parts.iter().filter(|p| p.is_some()).count()
    }

    /// Direct children of `parent`, in id order. The Animator and the rig panel
    /// both render the hierarchy by recursing on this.
    pub fn children(&self, parent: PartId) -> impl Iterator<Item = &Part> + '_ {
        self.iter().filter(move |p| p.parent == Some(parent))
    }

    /// Add a new part under `parent` with the given name, returning its new id.
    /// The pivot defaults to the parent's pivot so a fresh part starts at a sane
    /// joint location the user can then nudge.
    pub fn add(&mut self, name: impl Into<String>, parent: PartId) -> PartId {
        let pivot = self.get(parent).map(|p| p.pivot).unwrap_or(Vec3::ZERO);
        let id = PartId(self.parts.len() as u32);
        self.parts.push(Some(Part {
            id,
            name: name.into(),
            parent: Some(parent),
            pivot,
        }));
        id
    }

    /// Remove a part and re-parent its children onto its parent (so the subtree
    /// is preserved, not orphaned). The root cannot be removed. Returns the ids
    /// whose geometry must be dropped — only the deleted part itself, since its
    /// children survive. Returns an empty vec if the delete was rejected.
    pub fn remove(&mut self, id: PartId) -> Vec<PartId> {
        if id == self.root || self.get(id).is_none() {
            return Vec::new();
        }
        let new_parent = self.get(id).and_then(|p| p.parent);
        // Re-parent children onto the deleted part's parent.
        for slot in self.parts.iter_mut().flatten() {
            if slot.parent == Some(id) {
                slot.parent = new_parent;
            }
        }
        if let Some(slot) = self.parts.get_mut(id.0 as usize) {
            *slot = None;
        }
        vec![id]
    }

    /// Re-parent `id` under `new_parent`, rejecting the move if it would create a
    /// cycle (i.e. `new_parent` is `id` or a descendant of `id`) or touch the
    /// root. Returns `true` if the move was applied.
    pub fn reparent(&mut self, id: PartId, new_parent: PartId) -> bool {
        if id == self.root || id == new_parent || self.get(id).is_none() {
            return false;
        }
        if self.is_descendant(new_parent, id) {
            return false;
        }
        if let Some(part) = self.get_mut(id) {
            part.parent = Some(new_parent);
            true
        } else {
            false
        }
    }

    /// `true` if `maybe_descendant` is `ancestor` or below it in the tree.
    fn is_descendant(&self, maybe_descendant: PartId, ancestor: PartId) -> bool {
        let mut cur = Some(maybe_descendant);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.get(c).and_then(|p| p.parent);
        }
        false
    }
}

/// Seed the standard humanoid character rig: a `Torso` root with `Head` and four
/// limbs, each pivoted at its joint. Pivots assume a model roughly two blocks
/// tall standing on the build-volume floor; the UI lets the user re-pivot for
/// any proportions. This is the ONE place the default part set is defined.
pub fn default_character_parts() -> PartTree {
    // Pivots are in model-local world units (1 unit = 1 reference block). They
    // place each joint where a humanoid would articulate for a ~2-block figure.
    let torso = PartId(0);
    let parts = vec![
        Part {
            id: torso,
            name: "Torso".into(),
            parent: None,
            pivot: Vec3::new(0.5, 0.9, 0.5),
        },
        Part {
            id: PartId(1),
            name: "Head".into(),
            parent: Some(torso),
            pivot: Vec3::new(0.5, 1.5, 0.5),
        },
        Part {
            id: PartId(2),
            name: "RightArm".into(),
            parent: Some(torso),
            pivot: Vec3::new(0.15, 1.3, 0.5),
        },
        Part {
            id: PartId(3),
            name: "LeftArm".into(),
            parent: Some(torso),
            pivot: Vec3::new(0.85, 1.3, 0.5),
        },
        Part {
            id: PartId(4),
            name: "RightLeg".into(),
            parent: Some(torso),
            pivot: Vec3::new(0.35, 0.6, 0.5),
        },
        Part {
            id: PartId(5),
            name: "LeftLeg".into(),
            parent: Some(torso),
            pivot: Vec3::new(0.65, 0.6, 0.5),
        },
    ];
    PartTree::from_parts(parts)
}

/// Seed the default rig for an [`ObjectType::Item`]: a single `Body` part that
/// owns the whole model so every sub-voxel has a valid owner. The user can add
/// more parts under it (the rig editor works identically for items), so an item
/// can be split into independently-animatable pieces.
pub fn default_item_parts() -> PartTree {
    PartTree::from_parts(vec![Part {
        id: PartId(0),
        name: "Body".into(),
        parent: None,
        pivot: Vec3::new(0.5, 0.5, 0.5),
    }])
}

/// Seed the default rig for an object type.
pub fn default_parts(object: ObjectType) -> PartTree {
    match object {
        ObjectType::Character => default_character_parts(),
        ObjectType::Item => default_item_parts(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_seed_has_torso_root_and_five_children() {
        let tree = default_character_parts();
        assert_eq!(tree.len(), 6);
        let root = tree.root();
        assert_eq!(tree.get(root).map(|p| p.name.as_str()), Some("Torso"));
        assert_eq!(tree.children(root).count(), 5);
    }

    #[test]
    fn add_then_reparent_rejects_cycle() {
        let mut tree = default_character_parts();
        let torso = tree.root();
        let arm = PartId(2); // RightArm
        let hand = tree.add("RightHand", arm);
        // Hand under arm under torso — re-parenting the arm under its own hand
        // must be rejected (cycle).
        assert!(!tree.reparent(arm, hand));
        // But moving the hand under the torso directly is fine.
        assert!(tree.reparent(hand, torso));
        assert_eq!(tree.get(hand).and_then(|p| p.parent), Some(torso));
    }

    #[test]
    fn remove_reparents_children_and_protects_root() {
        let mut tree = default_character_parts();
        let torso = tree.root();
        let arm = PartId(2);
        let hand = tree.add("RightHand", arm);
        // Deleting the arm should re-parent the hand onto the torso.
        let removed = tree.remove(arm);
        assert_eq!(removed, vec![arm]);
        assert!(tree.get(arm).is_none());
        assert_eq!(tree.get(hand).and_then(|p| p.parent), Some(torso));
        // Root delete is rejected.
        assert!(tree.remove(torso).is_empty());
    }

    #[test]
    fn item_seeds_one_body_part_and_supports_multi_part() {
        // An item starts with a single `Body` root...
        let mut tree = default_item_parts();
        assert_eq!(tree.len(), 1);
        let body = tree.root();
        assert_eq!(tree.get(body).map(|p| p.name.as_str()), Some("Body"));

        // ...but supports the full multi-part rig editing a character does:
        // add, re-parent, and delete (children re-parent up).
        let lid = tree.add("Lid", body);
        let hinge = tree.add("Hinge", lid);
        assert_eq!(tree.len(), 3);
        assert_eq!(tree.get(hinge).and_then(|p| p.parent), Some(lid));
        // Cycle is still rejected for items.
        assert!(!tree.reparent(lid, hinge));
        // Deleting the lid re-parents the hinge onto the body.
        assert_eq!(tree.remove(lid), vec![lid]);
        assert_eq!(tree.get(hinge).and_then(|p| p.parent), Some(body));
        // The item root is protected just like a character's.
        assert!(tree.remove(body).is_empty());
    }
}
