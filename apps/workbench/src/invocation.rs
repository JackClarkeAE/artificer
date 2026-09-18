//! What a tool consumes, and how a selection resolves against it (ADR 0041).
//!
//! The workbench asks two questions about a command and used to answer them in
//! one place. They are different questions:
//!
//! - **Applicability** — can this operation happen at all? A staged operation,
//!   a history marker that is not at the tip, immutable library geometry. The
//!   ribbon asks only this, every frame, so it must stay cheap: it may never
//!   enumerate topology to decide whether to grey an icon.
//! - **Operand resolution** — do this command's inputs resolve from what is
//!   selected? Invocation asks this, once, when the button is pressed.
//!
//! Keeping them apart is what lets the invariant hold:
//!
//! > A command is never disabled merely because an operand has not been picked.
//!
//! Everything else here follows from that. A tool declares an *appetite*, and
//! invoking it resolves that appetite against the selection: enough already
//! selected and it stages, not enough and it arms and asks for the rest. The
//! two orders are one code path with two outcomes, which is the whole reason
//! preselection and tool-first cannot drift apart.
//!
//! This module is deliberately pure. It knows nothing about egui, the ribbon
//! or `KernelLabApp`; everything it needs from the document arrives through
//! [`InvocationContext`], so the resolver can be tested against a fake one.

use std::borrow::Cow;

use crate::viewport::{
    BodyInstanceKey, DocumentEdgeSelection, DocumentFaceSelection, DocumentVertexSelection,
};

/// One picked thing, whatever kind it is.
///
/// The workbench keeps a collection per kind, which cannot express the order a
/// heterogeneous selection was made in. Resolution works on this instead, so a
/// click sequence of face, edge, face, edge stays in that order rather than
/// becoming two independent lists.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SelectionItem {
    Body(BodyInstanceKey),
    Face(DocumentFaceSelection),
    Edge(DocumentEdgeSelection),
    Vertex(DocumentVertexSelection),
}

impl SelectionItem {
    /// A short word for the readout, so a tally can say what it ignored.
    #[must_use]
    pub const fn kind_label(self) -> &'static str {
        match self {
            Self::Body(_) => "body",
            Self::Face(_) => "face",
            Self::Edge(_) => "edge",
            Self::Vertex(_) => "vertex",
        }
    }

    /// The occurrence a pick belongs to.
    #[must_use]
    pub const fn body(self) -> BodyInstanceKey {
        match self {
            Self::Body(body) => body,
            Self::Face(face) => face.body,
            Self::Edge(edge) => edge.body,
            Self::Vertex(vertex) => vertex.body,
        }
    }
}

/// Whether a command can run at all, and in plain words why not.
///
/// This never depends on how much of the selection is present. A command that
/// is blocked here is blocked for a reason arming cannot fix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Applicability {
    Available,
    Blocked(Cow<'static, str>),
}

impl Applicability {
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn blocked(reason: impl Into<Cow<'static, str>>) -> Self {
        Self::Blocked(reason.into())
    }
}

/// Why one pick is or is not usable for one role.
///
/// The distinction between `WrongKind` and `Incompatible` is the difference
/// between ignoring a face a fillet cannot use and quietly rounding two of the
/// three edges the user thought they had picked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandEligibility {
    Accept,
    /// Not the sort of thing this role takes. Safe to set aside.
    WrongKind,
    /// The right sort of thing, which this operation cannot use.
    Incompatible(&'static str),
    /// The right sort of thing, on geometry that may not be edited.
    Immutable(&'static str),
    /// A reference the document no longer resolves.
    Stale,
}

/// How many of a thing a role takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cardinality {
    Exactly(usize),
    AtLeast(usize),
    Between(usize, usize),
}

impl Cardinality {
    #[must_use]
    pub const fn satisfied_by(self, count: usize) -> bool {
        match self {
            Self::Exactly(n) => count == n,
            Self::AtLeast(n) => count >= n,
            Self::Between(low, high) => count >= low && count <= high,
        }
    }

    /// Whether another operand could still be taken.
    #[must_use]
    pub const fn accepts_more(self, count: usize) -> bool {
        match self {
            Self::Exactly(n) | Self::Between(_, n) => count < n,
            Self::AtLeast(_) => true,
        }
    }
}

/// Where a role's operands may come from.
///
/// This exists because deterministic ordering may resolve *symmetric* operands
/// and must never invent *asymmetric* ones. Which edges a fillet rounds does
/// not depend on click order; which body a subtraction cuts very much does, and
/// "the first one selected" is a guess wearing a rule's clothing. The Boolean
/// code already carries that lesson: guessing an operand silently picked the
/// wrong body once a third existed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandSourcePolicy {
    /// Harvest freely from the selection in order. For operands where order
    /// carries no meaning.
    Symmetric,
    /// Take the workspace's active body, or an explicit pick. Never the first
    /// thing that happens to be selected.
    ActiveOrExplicit,
    /// Only an explicit pick made while the tool is armed.
    ExplicitOnly,
}

/// One operand a tool needs.
pub struct OperandRoleSpec {
    pub id: &'static str,
    /// What the prompt says while this role is the one being waited for.
    pub prompt: &'static str,
    pub cardinality: Cardinality,
    pub source: OperandSourcePolicy,
    /// Whether this pick can serve this role, given what is already bound.
    ///
    /// Eligibility is often relative: a Boolean's tool body is any body except
    /// its target, and a second chamfer edge must share a body with the first.
    pub accepts: fn(&dyn InvocationContext, &OperandBindings, SelectionItem) -> OperandEligibility,
}

/// One complete reading of a request: every role, needed together.
pub struct OperandSchema {
    pub roles: &'static [OperandRoleSpec],
}

/// A tool's appetite.
///
/// Alternatives are different readings of the same request, tried in order —
/// not roles. Mirror's plane may come from a construction plane, a planar face
/// or an origin plane, and those are three readings, not three operands it
/// needs at once.
pub struct ToolInvocationSpec {
    pub alternatives: &'static [OperandSchema],
}

/// What each role ended up bound to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OperandBindings {
    bound: Vec<(&'static str, Vec<SelectionItem>)>,
}

impl OperandBindings {
    #[must_use]
    pub fn get(&self, role: &str) -> &[SelectionItem] {
        self.bound
            .iter()
            .find(|(id, _)| *id == role)
            .map_or(&[], |(_, items)| items.as_slice())
    }

    fn push(&mut self, role: &'static str, item: SelectionItem) {
        if let Some((_, items)) = self.bound.iter_mut().find(|(id, _)| *id == role) {
            items.push(item);
        } else {
            self.bound.push((role, vec![item]));
        }
    }

    fn count(&self, role: &str) -> usize {
        self.get(role).len()
    }

    fn remove(&mut self, role: &str, item: SelectionItem) {
        if let Some((_, items)) = self.bound.iter_mut().find(|(id, _)| *id == role) {
            items.retain(|held| *held != item);
        }
    }

    /// Drops every binding the document no longer resolves, returning them so
    /// the readout can say what went.
    fn retain_resolvable(&mut self, context: &dyn InvocationContext) -> Vec<SelectionItem> {
        let mut lost = Vec::new();
        for (_, items) in &mut self.bound {
            items.retain(|item| {
                let resolves = context.resolves(*item);
                if !resolves {
                    lost.push(*item);
                }
                resolves
            });
        }
        lost
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bound.iter().all(|(_, items)| items.is_empty())
    }
}

/// What resolution set aside, and why.
///
/// Three classes, because they mean three different things to the user. An
/// extraneous pick is noise and is ignored; a rejected one is the right sort of
/// thing that this operation cannot use, and staging over it silently is how a
/// wrong part gets made; a stale one is a reference the document has lost.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolutionDiagnostics {
    pub extraneous: Vec<SelectionItem>,
    pub rejected: Vec<(SelectionItem, &'static str)>,
    pub stale: Vec<SelectionItem>,
}

impl ResolutionDiagnostics {
    #[must_use]
    pub fn blocks_staging(&self) -> bool {
        !self.rejected.is_empty()
    }

    /// The tally a readout shows beside what the tool is about to do.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.extraneous.is_empty() {
            parts.push(format!("{} ignored", self.extraneous.len()));
        }
        if !self.rejected.is_empty() {
            parts.push(format!("{} cannot be used", self.rejected.len()));
        }
        if !self.stale.is_empty() {
            parts.push(format!("{} no longer in the model", self.stale.len()));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// What invoking a tool against the current selection produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperandResolution {
    /// Every role is bound; the tool can stage.
    Complete {
        bindings: OperandBindings,
        diagnostics: ResolutionDiagnostics,
    },
    /// Some role still wants operands; the tool arms and asks for them.
    NeedsOperands {
        role: &'static str,
        prompt: &'static str,
        bindings: OperandBindings,
        diagnostics: ResolutionDiagnostics,
    },
    /// The selection holds something this operation cannot use. Staging over it
    /// would act on less than the user believes is selected.
    InvalidSelection(ResolutionDiagnostics),
}

/// What the resolver needs to know about the document.
///
/// A trait rather than a borrow of the app, so the resolver stays pure and its
/// tests can answer these questions however they like.
pub trait InvocationContext {
    /// The body the workspace is working on, if any.
    fn active_body(&self) -> Option<BodyInstanceKey>;
    /// Whether a reference still resolves in the current document.
    fn resolves(&self, item: SelectionItem) -> bool;
    /// Whether a body may be edited, or belongs to an immutable occurrence.
    fn body_is_editable(&self, body: BodyInstanceKey) -> bool;
    /// Whether a face is planar and carries the support a feature needs.
    fn face_is_planar_support(&self, face: DocumentFaceSelection) -> bool;
}

/// Resolves a selection against a tool's appetite.
///
/// The alternatives are tried in order and the first that resolves wins, so a
/// tool that reads a request two ways says which reading it took by which
/// alternative succeeded. A partial resolution is not a failure: it is the
/// tool-first case, and it arms holding whatever already fits.
#[must_use]
pub fn resolve(
    context: &dyn InvocationContext,
    spec: &ToolInvocationSpec,
    selection: &[SelectionItem],
) -> OperandResolution {
    let mut best: Option<OperandResolution> = None;
    for schema in spec.alternatives {
        let resolution = resolve_schema(context, schema, selection);
        match &resolution {
            OperandResolution::Complete { .. } => return resolution,
            // Keep the first partial answer: it is the reading the tool
            // prefers, and preference order is what decides between readings.
            OperandResolution::NeedsOperands { .. } if best.is_none() => {
                best = Some(resolution);
            }
            _ => {
                if best.is_none() {
                    best = Some(resolution);
                }
            }
        }
    }
    best.unwrap_or(OperandResolution::InvalidSelection(
        ResolutionDiagnostics::default(),
    ))
}

fn resolve_schema(
    context: &dyn InvocationContext,
    schema: &OperandSchema,
    selection: &[SelectionItem],
) -> OperandResolution {
    let mut bindings = OperandBindings::default();
    let mut diagnostics = ResolutionDiagnostics::default();

    // A stale reference is never offered to a role: the document has lost it,
    // and binding it would stage against something that is not there.
    let live: Vec<SelectionItem> = selection
        .iter()
        .copied()
        .filter(|item| {
            let resolves = context.resolves(*item);
            if !resolves {
                diagnostics.stale.push(*item);
            }
            resolves
        })
        .collect();

    // Roles whose operands may be harvested take them in selection order. A
    // role that may not be inferred from arbitrary order is left for the active
    // body or an explicit pick.
    let mut taken = vec![false; live.len()];
    for role in schema.roles {
        if role.source == OperandSourcePolicy::ExplicitOnly {
            continue;
        }
        if role.source == OperandSourcePolicy::ActiveOrExplicit {
            if let Some(body) = context.active_body() {
                let item = SelectionItem::Body(body);
                if matches!(
                    (role.accepts)(context, &bindings, item),
                    OperandEligibility::Accept
                ) {
                    bindings.push(role.id, item);
                }
            }
            continue;
        }
        for (index, item) in live.iter().copied().enumerate() {
            if taken[index] || !role.cardinality.accepts_more(bindings.count(role.id)) {
                continue;
            }
            match (role.accepts)(context, &bindings, item) {
                OperandEligibility::Accept => {
                    bindings.push(role.id, item);
                    taken[index] = true;
                }
                OperandEligibility::Incompatible(why) => {
                    // The right sort of thing that this operation cannot use.
                    // Recorded against the item, not skipped over.
                    if !diagnostics.rejected.iter().any(|(held, _)| *held == item) {
                        diagnostics.rejected.push((item, why));
                    }
                    taken[index] = true;
                }
                OperandEligibility::Immutable(why) => {
                    if !diagnostics.rejected.iter().any(|(held, _)| *held == item) {
                        diagnostics.rejected.push((item, why));
                    }
                    taken[index] = true;
                }
                OperandEligibility::Stale => {
                    diagnostics.stale.push(item);
                    taken[index] = true;
                }
                OperandEligibility::WrongKind => {}
            }
        }
    }

    // Whatever no role wanted is noise, and noise is ignored.
    for (index, item) in live.iter().copied().enumerate() {
        if !taken[index] {
            diagnostics.extraneous.push(item);
        }
    }

    if diagnostics.blocks_staging() {
        return OperandResolution::InvalidSelection(diagnostics);
    }
    for role in schema.roles {
        if !role.cardinality.satisfied_by(bindings.count(role.id)) {
            return OperandResolution::NeedsOperands {
                role: role.id,
                prompt: role.prompt,
                bindings,
                diagnostics,
            };
        }
    }
    OperandResolution::Complete {
        bindings,
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// The appetites themselves
// ---------------------------------------------------------------------------

/// Edges a finish can round or bevel: on one body, on a body that may be
/// edited, and all on the same body as one another.
fn accepts_finish_edge(
    context: &dyn InvocationContext,
    bindings: &OperandBindings,
    item: SelectionItem,
) -> OperandEligibility {
    let SelectionItem::Edge(edge) = item else {
        return OperandEligibility::WrongKind;
    };
    let body = edge.body;
    if !context.body_is_editable(body) {
        return OperandEligibility::Immutable("library component geometry is immutable");
    }
    // An edge finish works one body at a time. A second body's edge is the
    // right sort of thing and still cannot join this operation, so it is
    // rejected rather than ignored.
    if let Some(first) = bindings.get(EDGES_TO_FINISH).first()
        && first.body() != body
    {
        return OperandEligibility::Incompatible("edges must be on one body");
    }
    OperandEligibility::Accept
}

/// A planar face a feature can be built on.
fn accepts_planar_face(
    context: &dyn InvocationContext,
    _bindings: &OperandBindings,
    item: SelectionItem,
) -> OperandEligibility {
    let SelectionItem::Face(face) = item else {
        return OperandEligibility::WrongKind;
    };
    if !context.body_is_editable(face.body) {
        return OperandEligibility::Immutable("library component geometry is immutable");
    }
    if !context.face_is_planar_support(face) {
        return OperandEligibility::Incompatible("this feature needs a planar face");
    }
    OperandEligibility::Accept
}

/// A body a whole-body feature acts on.
fn accepts_editable_body(
    context: &dyn InvocationContext,
    _bindings: &OperandBindings,
    item: SelectionItem,
) -> OperandEligibility {
    let SelectionItem::Body(body) = item else {
        return OperandEligibility::WrongKind;
    };
    if !context.body_is_editable(body) {
        return OperandEligibility::Immutable("library component geometry is immutable");
    }
    OperandEligibility::Accept
}

pub const EDGES_TO_FINISH: &str = "edges";
pub const TARGET_FACE: &str = "face";
pub const TARGET_BODY: &str = "body";

/// Fillet and Chamfer: one or more edges, on one editable body.
pub const EDGE_FINISH: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[OperandRoleSpec {
            id: EDGES_TO_FINISH,
            prompt: "Pick the edges to finish",
            cardinality: Cardinality::AtLeast(1),
            source: OperandSourcePolicy::Symmetric,
            accepts: accepts_finish_edge,
        }],
    }],
};

/// Hole, Rib and Hole pattern: one planar face to build on.
pub const PLANAR_FACE_FEATURE: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[OperandRoleSpec {
            id: TARGET_FACE,
            prompt: "Pick a planar face",
            cardinality: Cardinality::Exactly(1),
            source: OperandSourcePolicy::Symmetric,
            accepts: accepts_planar_face,
        }],
    }],
};

/// Mirror, Pattern and Shell: one body, which the workspace's active body
/// satisfies without anything being picked.
pub const WHOLE_BODY_FEATURE: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[OperandRoleSpec {
            id: TARGET_BODY,
            prompt: "Pick a body",
            cardinality: Cardinality::Exactly(1),
            source: OperandSourcePolicy::ActiveOrExplicit,
            accepts: accepts_editable_body,
        }],
    }],
};

// ---------------------------------------------------------------------------
// A tool waiting to be told what to work on (ADR 0041 stage 3)
// ---------------------------------------------------------------------------

/// A tool that has been pressed and is collecting the operands it still needs.
///
/// Armed is not staged. A staged operation owns the ribbon and awaits
/// confirmation; an armed tool has changed no document state at all, so it
/// needs no undo entry, another tool press simply replaces it, and Escape
/// drops it. The bindings live here rather than in the selection because the
/// tool owns them and the user owns the selection — which is why nothing has
/// to be "restored" when a tool is dropped.
pub struct ArmedTool {
    pub tool: &'static str,
    schema: &'static OperandSchema,
    bindings: OperandBindings,
    diagnostics: ResolutionDiagnostics,
    /// The document revision these bindings were resolved against. They are
    /// re-resolved when it changes, not because a frame was painted.
    resolved_at: u64,
}

impl ArmedTool {
    /// Arms a tool, keeping whatever the selection already satisfies.
    #[must_use]
    pub fn arm(
        tool: &'static str,
        schema: &'static OperandSchema,
        bindings: OperandBindings,
        diagnostics: ResolutionDiagnostics,
        revision: u64,
    ) -> Self {
        Self {
            tool,
            schema,
            bindings,
            diagnostics,
            resolved_at: revision,
        }
    }

    #[must_use]
    pub const fn bindings(&self) -> &OperandBindings {
        &self.bindings
    }

    #[must_use]
    pub const fn diagnostics(&self) -> &ResolutionDiagnostics {
        &self.diagnostics
    }

    /// The role still waiting, and what to ask for. `None` once every role is
    /// satisfied and the tool is ready to stage.
    #[must_use]
    pub fn waiting_for(&self) -> Option<&'static OperandRoleSpec> {
        self.schema
            .roles
            .iter()
            .find(|role| !role.cardinality.satisfied_by(self.bindings.count(role.id)))
    }

    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.waiting_for().is_none()
    }

    /// What the status line says while this tool waits.
    #[must_use]
    pub fn prompt(&self) -> String {
        let Some(role) = self.waiting_for() else {
            // Ready still owes the tally: a tool that lost an operand to a
            // rebuild must say so rather than quietly staging with fewer.
            return match self.diagnostics.summary() {
                Some(tally) => format!("{} · ready · {tally}", self.tool),
                None => format!("{} · ready", self.tool),
            };
        };
        let held = self.bindings.count(role.id);
        let base = match role.cardinality {
            Cardinality::Exactly(n) if n > 1 => {
                format!("{} · {} ({held} of {n})", self.tool, role.prompt)
            }
            Cardinality::Between(_, high) => {
                format!("{} · {} ({held} of {high})", self.tool, role.prompt)
            }
            _ if held > 0 => format!("{} · {} ({held} so far)", self.tool, role.prompt),
            _ => format!("{} · {}", self.tool, role.prompt),
        };
        match self.diagnostics.summary() {
            Some(tally) => format!("{base} · {tally}"),
            None => base,
        }
    }

    /// The role that would take the next pick.
    ///
    /// Not the same as the role being waited for. A fillet asking for "one or
    /// more edges" is *satisfied* by the first one and still wants the rest,
    /// so a tool can be ready to stage and taking picks at the same time.
    #[must_use]
    fn taking(&self) -> Option<&'static OperandRoleSpec> {
        self.waiting_for().or_else(|| {
            self.schema
                .roles
                .iter()
                .rev()
                .find(|role| role.cardinality.accepts_more(self.bindings.count(role.id)))
        })
    }

    /// Whether a pick could serve the role currently being waited for.
    ///
    /// This is what narrows the viewport's hit test: an armed tool masks
    /// picking to the candidates it can actually use, so aiming at the wrong
    /// kind of thing is impossible rather than merely futile.
    #[must_use]
    pub fn accepts(&self, context: &dyn InvocationContext, item: SelectionItem) -> bool {
        self.taking().is_some_and(|role| {
            matches!(
                (role.accepts)(context, &self.bindings, item),
                OperandEligibility::Accept
            )
        })
    }

    /// Offers a pick to the role being waited for. Returns whether it bound.
    pub fn offer(&mut self, context: &dyn InvocationContext, item: SelectionItem) -> bool {
        let Some(role) = self.taking() else {
            return false;
        };
        // Clicking a bound operand again takes it back off, which is how every
        // multi-pick tool in the workbench already behaves.
        if self.bindings.get(role.id).contains(&item) {
            self.bindings.remove(role.id, item);
            return true;
        }
        match (role.accepts)(context, &self.bindings, item) {
            OperandEligibility::Accept => {
                self.bindings.push(role.id, item);
                true
            }
            OperandEligibility::Incompatible(why) | OperandEligibility::Immutable(why) => {
                self.diagnostics.rejected.push((item, why));
                false
            }
            OperandEligibility::WrongKind | OperandEligibility::Stale => false,
        }
    }

    /// Re-resolves against the document when it has moved on.
    ///
    /// References go stale under a rebuild, an undo or a history move, and a
    /// tool must never stage against one it cannot resolve. This runs when the
    /// revision changes and once more immediately before staging — not because
    /// egui painted another frame.
    pub fn revalidate(&mut self, context: &dyn InvocationContext, revision: u64) {
        if self.resolved_at == revision {
            return;
        }
        self.resolved_at = revision;
        let lost = self.bindings.retain_resolvable(context);
        self.diagnostics.stale.extend(lost);
    }
}

/// The body a Boolean cuts or joins into.
fn accepts_boolean_target(
    context: &dyn InvocationContext,
    _bindings: &OperandBindings,
    item: SelectionItem,
) -> OperandEligibility {
    accepts_editable_body(context, _bindings, item)
}

/// Any body except the target.
fn accepts_boolean_tool(
    context: &dyn InvocationContext,
    bindings: &OperandBindings,
    item: SelectionItem,
) -> OperandEligibility {
    let SelectionItem::Body(body) = item else {
        return OperandEligibility::WrongKind;
    };
    if bindings.get(BOOLEAN_TARGET).contains(&item) {
        return OperandEligibility::Incompatible("the target body cannot also be a tool");
    }
    accepts_editable_body(context, bindings, SelectionItem::Body(body))
}

pub const BOOLEAN_TARGET: &str = "target";
pub const BOOLEAN_TOOLS: &str = "tools";

/// A body Boolean: the body being cut or joined into, and the bodies doing it.
///
/// The two roles are asymmetric and are sourced accordingly. The target comes
/// from the active body or an explicit pick, never from whatever happens to be
/// first in the selection; the tools come only from picks made while the
/// operation is staged. The workbench's own Boolean code carries the reason in
/// a comment — guessing an operand silently picked the wrong body once a third
/// existed — and this is that lesson written as a rule the resolver enforces.
pub const BODY_BOOLEAN: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[
            OperandRoleSpec {
                id: BOOLEAN_TARGET,
                prompt: "Pick the body to cut into",
                cardinality: Cardinality::Exactly(1),
                source: OperandSourcePolicy::ActiveOrExplicit,
                accepts: accepts_boolean_target,
            },
            OperandRoleSpec {
                id: BOOLEAN_TOOLS,
                prompt: "Pick the bodies to cut with",
                cardinality: Cardinality::AtLeast(1),
                source: OperandSourcePolicy::ExplicitOnly,
                accepts: accepts_boolean_tool,
            },
        ],
    }],
};

pub const PUSH_PULL_FACE: &str = "face";

/// Pushing or pulling a face: one planar cap to move.
///
/// Extrude serves two operations, and this is the one that acts on a body. Its
/// face is an operand the user picks, so pressing Extrude in the model
/// workspace with nothing picked asks for it rather than greying out — which is
/// the same thing the *sketch* half of Extrude has always done for a profile.
pub const FACE_PUSH_PULL: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[OperandRoleSpec {
            id: PUSH_PULL_FACE,
            prompt: "Pick the face to push or pull",
            cardinality: Cardinality::Exactly(1),
            source: OperandSourcePolicy::Symmetric,
            accepts: accepts_planar_face,
        }],
    }],
};

pub const PROFILE_REGION: &str = "profile";

/// Extruding a sketch: the closed region to raise or cut with.
///
/// Extrude already behaves this way — its availability function says so in a
/// comment, that it hands the canvas to Select and says where to click rather
/// than greying out — so this records the appetite that behaviour implies.
pub const SKETCH_EXTRUSION: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[OperandRoleSpec {
            id: PROFILE_REGION,
            prompt: "Pick the profile to extrude",
            cardinality: Cardinality::AtLeast(1),
            source: OperandSourcePolicy::ExplicitOnly,
            accepts: |_, _, _| OperandEligibility::WrongKind,
        }],
    }],
};

pub const RELATION_OPERANDS: &str = "operands";

/// A sketch relation's operands: endpoints, whole curves, or a mix.
///
/// The sketch toolbar has worked this way since relations existed — arm the
/// relation, then pick what it applies to — and is the closest thing this
/// codebase had to a reference implementation before any of this was written.
/// Recording its appetite here is what lets the sketch and the model say the
/// same thing about operands; the sketch keeps its own state machine, which
/// lives inside the sketch-tool state and has a canvas of its own.
///
/// Relation operands are symmetric: which endpoint of a coincident pair was
/// clicked first carries no meaning, and neither does the order two lines were
/// picked in for a parallel. Where a relation *is* asymmetric — a distance
/// measured from one end, whose retype holds that end still — the held end is
/// named explicitly rather than taken from the order, exactly as ADR 0038's
/// amendment requires.
pub const SKETCH_RELATION: ToolInvocationSpec = ToolInvocationSpec {
    alternatives: &[OperandSchema {
        roles: &[OperandRoleSpec {
            id: RELATION_OPERANDS,
            prompt: "Pick the sketch geometry the relation applies to",
            cardinality: Cardinality::Between(2, 2),
            source: OperandSourcePolicy::Symmetric,
            accepts: |_, _, _| OperandEligibility::WrongKind,
        }],
    }],
};

#[cfg(test)]
mod tests {
    use super::*;
    use artificer_protocol::{EntityId, EntityKind, EntityRef, SnapshotId};

    /// A document that answers however the test needs it to.
    #[derive(Default)]
    struct Fake {
        active: Option<BodyInstanceKey>,
        immutable: Vec<u64>,
        curved_faces: Vec<u64>,
        missing: Vec<SelectionItem>,
    }

    impl InvocationContext for Fake {
        fn active_body(&self) -> Option<BodyInstanceKey> {
            self.active
        }
        fn resolves(&self, item: SelectionItem) -> bool {
            !self.missing.contains(&item)
        }
        fn body_is_editable(&self, body: BodyInstanceKey) -> bool {
            !self.immutable.contains(&body.get())
        }
        fn face_is_planar_support(&self, face: DocumentFaceSelection) -> bool {
            !self.curved_faces.contains(&face.face.entity.0)
        }
    }

    fn body(id: u64) -> BodyInstanceKey {
        BodyInstanceKey::new(id)
    }

    fn reference(entity: u64, kind: EntityKind) -> EntityRef {
        EntityRef {
            snapshot: SnapshotId::new([1; 16]),
            entity: EntityId(entity),
            kind,
        }
    }

    fn edge(body_id: u64, entity: u64) -> SelectionItem {
        SelectionItem::Edge(DocumentEdgeSelection {
            body: BodyInstanceKey::new(body_id),
            edge: reference(entity, EntityKind::Edge),
        })
    }

    fn face(body_id: u64, entity: u64) -> SelectionItem {
        SelectionItem::Face(DocumentFaceSelection {
            body: BodyInstanceKey::new(body_id),
            face: reference(entity, EntityKind::Face),
        })
    }

    /// The tool-first case. Nothing selected is not a failure; it is a request
    /// for operands, which is the whole point of ADR 0041.
    #[test]
    fn an_empty_selection_asks_for_operands_rather_than_failing() {
        let context = Fake::default();
        let resolution = resolve(&context, &EDGE_FINISH, &[]);
        let OperandResolution::NeedsOperands { role, prompt, .. } = resolution else {
            panic!("an empty selection should ask for edges: {resolution:?}");
        };
        assert_eq!(role, EDGES_TO_FINISH);
        assert_eq!(prompt, "Pick the edges to finish");
    }

    /// The selection-first case, and the user's own example: two edges and a
    /// face, pressed Chamfer. The edges bind, the face is noise.
    #[test]
    fn a_mixed_selection_binds_what_fits_and_ignores_the_rest() {
        let context = Fake::default();
        let selection = [edge(1, 10), face(1, 20), edge(1, 11)];
        let OperandResolution::Complete {
            bindings,
            diagnostics,
        } = resolve(&context, &EDGE_FINISH, &selection)
        else {
            panic!("two edges satisfy an edge finish");
        };
        assert_eq!(bindings.get(EDGES_TO_FINISH).len(), 2);
        assert_eq!(diagnostics.extraneous, vec![face(1, 20)]);
        assert!(diagnostics.rejected.is_empty());
        assert_eq!(diagnostics.summary().as_deref(), Some("1 ignored"));
    }

    /// The line the review drew, and the important one. A face a fillet cannot
    /// use is noise. An *edge* it cannot use is not: rounding the other two and
    /// saying "1 ignored" is how a wrong part gets made.
    #[test]
    fn an_unusable_operand_of_the_right_kind_blocks_staging() {
        let context = Fake::default();
        // An edge on a second body is the right sort of thing and still cannot
        // join a finish that has already bound one body's edges.
        let selection = [edge(1, 10), edge(2, 30)];
        let resolution = resolve(&context, &EDGE_FINISH, &selection);
        let OperandResolution::InvalidSelection(diagnostics) = resolution else {
            panic!("an edge that cannot be used must not stage silently: {resolution:?}");
        };
        assert_eq!(diagnostics.rejected.len(), 1);
        assert_eq!(diagnostics.rejected[0].0, edge(2, 30));
        assert!(diagnostics.extraneous.is_empty());
    }

    /// A reference the document has lost is removed and reported, never bound.
    #[test]
    fn a_stale_reference_is_dropped_and_named() {
        let gone = edge(1, 99);
        let context = Fake {
            missing: vec![gone],
            ..Fake::default()
        };
        let OperandResolution::Complete {
            bindings,
            diagnostics,
        } = resolve(&context, &EDGE_FINISH, &[edge(1, 10), gone])
        else {
            panic!("the live edge still satisfies the finish");
        };
        assert_eq!(bindings.get(EDGES_TO_FINISH).len(), 1);
        assert_eq!(diagnostics.stale, vec![gone]);
        assert_eq!(
            diagnostics.summary().as_deref(),
            Some("1 no longer in the model")
        );
    }

    /// Immutable geometry is rejected rather than ignored: the user picked the
    /// right sort of thing and is owed the reason it cannot be used.
    #[test]
    fn immutable_geometry_is_rejected_with_its_reason() {
        let context = Fake {
            immutable: vec![1],
            ..Fake::default()
        };
        let resolution = resolve(&context, &EDGE_FINISH, &[edge(1, 10)]);
        let OperandResolution::InvalidSelection(diagnostics) = resolution else {
            panic!("an immutable edge cannot be finished: {resolution:?}");
        };
        assert_eq!(
            diagnostics.rejected[0].1,
            "library component geometry is immutable"
        );
    }

    /// A curved face is the right kind and the wrong geometry, so a face
    /// feature refuses rather than building on it.
    #[test]
    fn a_curved_face_cannot_carry_a_planar_feature() {
        let context = Fake {
            curved_faces: vec![20],
            ..Fake::default()
        };
        let resolution = resolve(&context, &PLANAR_FACE_FEATURE, &[face(1, 20)]);
        let OperandResolution::InvalidSelection(diagnostics) = resolution else {
            panic!("a curved face cannot host a planar feature: {resolution:?}");
        };
        assert_eq!(
            diagnostics.rejected[0].1,
            "this feature needs a planar face"
        );
    }

    /// A whole-body feature is satisfied by the workspace's active body without
    /// anything being picked — which is exactly today's behaviour, expressed as
    /// an appetite rather than as a gate.
    #[test]
    fn a_whole_body_feature_takes_the_active_body_with_nothing_selected() {
        let context = Fake {
            active: Some(body(1)),
            ..Fake::default()
        };
        let OperandResolution::Complete { bindings, .. } =
            resolve(&context, &WHOLE_BODY_FEATURE, &[])
        else {
            panic!("the active body satisfies a whole-body feature");
        };
        assert_eq!(bindings.get(TARGET_BODY), &[SelectionItem::Body(body(1))]);
    }

    /// With no active body it asks, rather than taking whatever body happens to
    /// be selected first.
    #[test]
    fn a_whole_body_feature_asks_rather_than_guessing() {
        let context = Fake::default();
        let resolution = resolve(
            &context,
            &WHOLE_BODY_FEATURE,
            &[SelectionItem::Body(body(7))],
        );
        let OperandResolution::NeedsOperands { role, .. } = resolution else {
            panic!("an asymmetric role must not be inferred from selection order: {resolution:?}");
        };
        assert_eq!(role, TARGET_BODY);
    }

    /// A bound operand the document has lost is dropped and reported when the
    /// revision moves, not left to be staged against. And re-resolution happens
    /// because the document changed, never because a frame was painted.
    #[test]
    fn an_armed_tool_drops_bindings_the_document_has_lost() {
        let mut context = Fake::default();
        let held = edge(1, 10);
        let OperandResolution::Complete {
            bindings,
            diagnostics,
        } = resolve(&context, &EDGE_FINISH, &[held, edge(1, 11)])
        else {
            panic!("two edges satisfy a finish");
        };
        let mut armed = ArmedTool::arm(
            "Fillet",
            &EDGE_FINISH.alternatives[0],
            bindings,
            diagnostics,
            1,
        );
        assert_eq!(armed.bindings().get(EDGES_TO_FINISH).len(), 2);

        // The same revision does no work at all.
        context.missing = vec![held];
        armed.revalidate(&context, 1);
        assert_eq!(
            armed.bindings().get(EDGES_TO_FINISH).len(),
            2,
            "nothing is re-resolved while the document has not moved"
        );

        // A new revision drops what no longer resolves and says so.
        armed.revalidate(&context, 2);
        assert_eq!(armed.bindings().get(EDGES_TO_FINISH).len(), 1);
        assert_eq!(armed.diagnostics().stale, vec![held]);
        assert!(
            armed.prompt().contains("no longer in the model"),
            "the readout should say what went: {}",
            armed.prompt()
        );
    }

    /// Clicking a bound operand again takes it back off, which is how every
    /// multi-pick tool in the workbench already behaves.
    #[test]
    fn offering_a_bound_operand_again_takes_it_off() {
        let context = Fake::default();
        let mut armed = ArmedTool::arm(
            "Fillet",
            &EDGE_FINISH.alternatives[0],
            OperandBindings::default(),
            ResolutionDiagnostics::default(),
            1,
        );
        let held = edge(1, 10);
        assert!(armed.offer(&context, held));
        assert_eq!(armed.bindings().get(EDGES_TO_FINISH), &[held]);
        assert!(armed.offer(&context, held));
        assert!(armed.bindings().get(EDGES_TO_FINISH).is_empty());
    }

    /// An armed tool narrows what can be picked to what it can use, so aiming
    /// at the wrong sort of thing is impossible rather than merely futile.
    #[test]
    fn an_armed_tool_only_accepts_what_its_role_can_use() {
        let context = Fake::default();
        let armed = ArmedTool::arm(
            "Fillet",
            &EDGE_FINISH.alternatives[0],
            OperandBindings::default(),
            ResolutionDiagnostics::default(),
            1,
        );
        assert!(armed.accepts(&context, edge(1, 10)));
        assert!(!armed.accepts(&context, face(1, 20)));
    }

    /// The rule the Boolean code learned the hard way: a target is never taken
    /// from whatever happens to be first in the selection.
    #[test]
    fn a_boolean_target_is_never_inferred_from_selection_order() {
        // Two bodies selected and no active body. A resolver that harvested in
        // order would call the first one the target and silently cut the wrong
        // one once a third existed.
        let context = Fake::default();
        let resolution = resolve(
            &context,
            &BODY_BOOLEAN,
            &[SelectionItem::Body(body(7)), SelectionItem::Body(body(8))],
        );
        let OperandResolution::NeedsOperands { role, .. } = resolution else {
            panic!("an asymmetric role must be asked for, not guessed: {resolution:?}");
        };
        assert_eq!(role, BOOLEAN_TARGET);
    }

    /// With an active body the target is known, and the tools are still asked
    /// for rather than taken from the selection.
    #[test]
    fn a_boolean_takes_its_target_from_the_active_body_and_asks_for_tools() {
        let context = Fake {
            active: Some(body(1)),
            ..Fake::default()
        };
        let resolution = resolve(&context, &BODY_BOOLEAN, &[SelectionItem::Body(body(2))]);
        let OperandResolution::NeedsOperands { role, bindings, .. } = resolution else {
            panic!("the tools are still outstanding: {resolution:?}");
        };
        assert_eq!(role, BOOLEAN_TOOLS);
        assert_eq!(
            bindings.get(BOOLEAN_TARGET),
            &[SelectionItem::Body(body(1))],
            "the target is the active body, not the selected one"
        );
    }

    /// The target cannot also be a tool, and saying so is a rejection with a
    /// reason rather than a silent omission.
    #[test]
    fn a_booleans_target_cannot_also_be_one_of_its_tools() {
        let context = Fake::default();
        let mut bindings = OperandBindings::default();
        bindings.push(BOOLEAN_TARGET, SelectionItem::Body(body(1)));
        assert!(matches!(
            accepts_boolean_tool(&context, &bindings, SelectionItem::Body(body(1))),
            OperandEligibility::Incompatible(_)
        ));
        assert!(matches!(
            accepts_boolean_tool(&context, &bindings, SelectionItem::Body(body(2))),
            OperandEligibility::Accept
        ));
    }

    /// A relation takes exactly two operands and stops asking once it has
    /// them, which is what "between two and two" says and what the sketch
    /// toolbar has always done.
    #[test]
    fn a_sketch_relation_wants_exactly_two_operands() {
        let role = &SKETCH_RELATION.alternatives[0].roles[0];
        assert!(!role.cardinality.satisfied_by(0));
        assert!(!role.cardinality.satisfied_by(1));
        assert!(role.cardinality.satisfied_by(2));
        assert!(
            !role.cardinality.accepts_more(2),
            "a third pick starts a new relation rather than joining this one"
        );
    }

    /// Symmetric operands do not care what order they were picked in, which is
    /// what makes a rubber-band selection safe for them.
    #[test]
    fn symmetric_operands_resolve_the_same_whatever_the_order() {
        let context = Fake::default();
        let forward = resolve(&context, &EDGE_FINISH, &[edge(1, 10), edge(1, 11)]);
        let reversed = resolve(&context, &EDGE_FINISH, &[edge(1, 11), edge(1, 10)]);
        let (
            OperandResolution::Complete { bindings: a, .. },
            OperandResolution::Complete { bindings: b, .. },
        ) = (forward, reversed)
        else {
            panic!("both orders should resolve");
        };
        let mut first = a.get(EDGES_TO_FINISH).to_vec();
        let mut second = b.get(EDGES_TO_FINISH).to_vec();
        first.sort_by_key(|item| format!("{item:?}"));
        second.sort_by_key(|item| format!("{item:?}"));
        assert_eq!(first, second, "order must not change a symmetric binding");
    }
}
