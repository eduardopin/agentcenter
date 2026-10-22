//! Normalized UI boundary, not a transport or a runtime provider.
//! The adapter must revalidate identity/revision atomically in the core too.
use adw::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Handoff,
    Compact,
    /// Composite handoff, new session in the same pane, pickup (CC5/CC6).
    Pickup,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Handoff => "Handoff",
            Self::Compact => "Compact",
            Self::Pickup => "Handoff + new session",
        }
    }
}

/// Which native adapter owns the session. Decides evidence and resume route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Pi,
    Claude,
}

/// Closed reason codes the panel turns into its single line of text.
pub mod reason {
    /// Pi runs outside herdr: Pi only allows a new session inside a command,
    /// which CC5 sends through the herdr pane (owner, 22/10).
    pub const PI_OUTSIDE_HERDR: &str = "pi_outside_herdr";
    /// The composite pickup adapter (CC5 Pi, CC6 Claude) is not in this build.
    pub const PICKUP_NOT_INTEGRATED: &str = "pickup_not_integrated";
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub agent: Agent,
    /// Opaque native session identity, never a PID or display name alone.
    pub identity: String,
    pub revision: String,
    pub name: String,
    /// Fixed private directory; the producer chooses the nonce-prefixed filename.
    pub handoff_destination: String,
    pub expected: serde_json::Value,
    /// None means supported; otherwise show this reason inline, not only a tooltip.
    pub handoff_disabled: Option<String>,
    pub compact_disabled: Option<String>,
    pub pickup_disabled: Option<String>,
    pub cancel_supported: bool,
}
impl Session {
    /// Converts authenticated protocol-2 metadata only. No discovery or transport fallback.
    pub fn from_metadata(
        metadata: &serde_json::Value,
        name: String,
        directory: String,
    ) -> Option<Self> {
        let expected = metadata.get("expected")?;
        let manager = expected.get("managerId")?.as_str()?;
        let id = expected.get("sessionId")?.as_str()?;
        let file = expected.get("sessionFile")?.as_str()?;
        let revision = expected.get("revision")?.as_str()?;
        if manager.is_empty()
            || id.is_empty()
            || file.is_empty()
            || revision.is_empty()
            || !revision.bytes().all(|b| b.is_ascii_digit())
            || (revision.len() > 1 && revision.starts_with('0'))
            || !matches!(
                expected.get("leafId"),
                Some(serde_json::Value::Null | serde_json::Value::String(_))
            )
            || !canonical_absolute(&directory)
        {
            return None;
        }
        let reason = |action: &str| -> Option<Option<String>> {
            let cap = metadata.get("capabilities")?.get(action)?;
            let enabled = cap.get("enabled")?.as_bool()?;
            Some(if enabled {
                None
            } else {
                Some(
                    cap.get("disabled_reason")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Unavailable")
                        .to_owned(),
                )
            })
        };
        Some(Self {
            agent: Agent::Pi,
            identity: serde_json::json!([manager, id, file]).to_string(),
            revision: revision.to_owned(),
            name,
            handoff_destination: directory,
            expected: expected.clone(),
            handoff_disabled: reason("handoff")?,
            compact_disabled: reason("compact")?,
            pickup_disabled: Some(reason::PICKUP_NOT_INTEGRATED.into()),
            cancel_supported: ["handoff", "compact"].iter().all(|action| {
                metadata["capabilities"][action]["cancel_supported"].as_bool() == Some(true)
            }),
        })
    }
    pub fn disabled_reason(&self, action: Action) -> Option<&str> {
        if self.identity.is_empty() || self.revision.is_empty() {
            return Some("Session identity or revision is unavailable");
        }
        if self.agent == Agent::Pi
            && action == Action::Handoff
            && self.handoff_destination.is_empty()
        {
            return Some("Private handoff destination is unavailable");
        }
        match action {
            Action::Handoff => self.handoff_disabled.as_deref(),
            Action::Compact => self.compact_disabled.as_deref(),
            Action::Pickup => self.pickup_disabled.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirmation {
    session: Session,
    action: Action,
    generation: u64,
}
impl Session {
    /// How the owner resumes the old session by hand if a pickup stops.
    pub fn resume_route(&self) -> String {
        let id = self.expected["sessionId"].as_str().unwrap_or("");
        match self.agent {
            Agent::Pi => format!(
                "open the old session file {} in Pi",
                self.expected["sessionFile"].as_str().unwrap_or("")
            ),
            Agent::Claude => format!("run claude --resume {id}"),
        }
    }
}
impl Confirmation {
    pub fn body(&self) -> String {
        let s = &self.session;
        let effect = match (self.action, s.agent) {
            (Action::Handoff, Agent::Pi) => format!(
                "Sends projected session text to the provider currently selected in Pi to generate a handoff. Saves it in the private directory {}. The producer chooses the filename; the GUI does not read its content.",
                s.handoff_destination
            ),
            (Action::Handoff, Agent::Claude) => "Types /clear-pickup handoff into this Claude Code pane in herdr. Claude writes the handoff file; the GUI records its path, size and sha256, never its content.".into(),
            (Action::Compact, Agent::Pi) => {
                "Compacts native context. The raw native history remains available.".into()
            }
            (Action::Compact, Agent::Claude) => "Types /compact into this Claude Code pane in herdr. The transcript keeps the raw history.".into(),
            (Action::Pickup, _) => format!(
                "1. Writes a handoff and waits for it to be saved.\n2. Starts a new session in the same pane.\n3. Loads the handoff into the new session.\nThe old session's history stays on disk. If a step fails or its result is unknown, the old session is kept: {}.",
                s.resume_route()
            ),
        };
        let scope = if self.action == Action::Pickup {
            "Clears this pane into a new session only after the handoff is saved."
        } else {
            "Does not clear, create, close, shut down or resume a session."
        };
        format!(
            "Session: {}\nIdentity: {}\nRevision: {}\n\n{effect}\nThis may consume tokens with the current model. {scope}",
            s.name, s.identity, s.revision
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub operation_id: String,
    pub session: Session,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    Handoff {
        path: String,
        expected: serde_json::Value,
    },
    Compact {
        capability: String,
        session_id: String,
        session_file: String,
        previous_leaf_id: serde_json::Value,
        entry_id: String,
    },
    /// Claude handoff file verified by the core: identity and digest only.
    Persisted {
        path: String,
        size: u64,
        sha256: String,
    },
    /// Claude manual `compact_boundary`, epoch ms.
    Compacted { boundary_ms: u64 },
    /// Pickup reached pickup_loaded: the new session and the handoff it read.
    Picked {
        new_session_id: String,
        handoff_path: String,
        sha256: String,
    },
}
// Lexical validation only: filesystem authentication remains the core's job.
fn canonical_absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() > 1
        && !value.contains('\0')
        && value[1..]
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}
impl Evidence {
    fn valid_for(&self, request: &Request) -> bool {
        let s = &request.session;
        if let Self::Picked {
            new_session_id,
            handoff_path,
            sha256,
        } = self
        {
            return request.action == Action::Pickup
                && !new_session_id.is_empty()
                && s.expected["sessionId"].as_str() != Some(new_session_id.as_str())
                && canonical_absolute(handoff_path)
                && sha256.len() == 64
                && sha256.bytes().all(|b| b.is_ascii_hexdigit());
        }
        if request.action == Action::Pickup {
            return false;
        }
        if s.agent == Agent::Claude {
            return match (request.action, self) {
                (Action::Handoff, Self::Persisted { path, sha256, .. }) => {
                    canonical_absolute(path) && sha256.len() == 64
                }
                (Action::Compact, Self::Compacted { boundary_ms }) => *boundary_ms > 0,
                _ => false,
            };
        }
        match (request.action, self) {
            (Action::Handoff, Self::Handoff { path, expected }) => {
                let p = std::path::Path::new(path);
                canonical_absolute(path)
                    && canonical_absolute(&s.handoff_destination)
                    && p.parent() == Some(std::path::Path::new(&s.handoff_destination))
                    && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        let suffix = format!("-{}.md", request.operation_id);
                        n.len() > suffix.len() && n.ends_with(&suffix)
                    })
                    && expected == &s.expected
            }
            (
                Action::Compact,
                Self::Compact {
                    capability,
                    session_id,
                    session_file,
                    previous_leaf_id,
                    entry_id,
                },
            ) => {
                capability == "agentcenter.compactGuarded.v1"
                    && s.expected.get("sessionId").and_then(|v| v.as_str())
                        == Some(session_id.as_str())
                    && s.expected.get("sessionFile").and_then(|v| v.as_str())
                        == Some(session_file.as_str())
                    && s.expected.get("leafId") == Some(previous_leaf_id)
                    && !entry_id.is_empty()
                    && previous_leaf_id.as_str() != Some(entry_id.as_str())
            }
            _ => false,
        }
    }
}
/// Map values already authenticated by the protocol-2 core. Never sends a request.
/// Keep the original request for status/cancel, not metadata.expected from a reply.
pub fn status_from_reply(request: &Request, reply: &serde_json::Value) -> Status {
    if reply.get("protocol_version").and_then(|v| v.as_u64()) != Some(2)
        || reply.get("operation_id").and_then(|v| v.as_str()) != Some(request.operation_id.as_str())
    {
        return Status::Unknown;
    }
    let code = reply.get("code").and_then(|v| v.as_str()).unwrap_or("");
    match reply.get("state").and_then(|v| v.as_str()) {
        Some("accepted") if code == "accepted" => Status::Accepted,
        Some("running") if matches!(code, "generating" | "compacting") => Status::Running,
        Some("rejected") => Status::Refused(code.into()),
        Some("failed") => Status::Failed(code.into()),
        Some("cancelled") => Status::Cancelled,
        Some("completed") if reply.get("metadata").is_some_and(|m| m.is_object()) => {
            let evidence = if code == "persisted" && request.action == Action::Handoff {
                reply
                    .get("artifact_path")
                    .and_then(|v| v.as_str())
                    .zip(reply.get("expected"))
                    .map(|(path, expected)| Evidence::Handoff {
                        path: path.into(),
                        expected: expected.clone(),
                    })
            } else if code == "compacted" && request.action == Action::Compact {
                let c = &reply["compaction"];
                (|| {
                    Some(Evidence::Compact {
                        capability: c.get("capability")?.as_str()?.into(),
                        session_id: c.get("sessionId")?.as_str()?.into(),
                        session_file: c.get("sessionFile")?.as_str()?.into(),
                        previous_leaf_id: c.get("previousLeafId")?.clone(),
                        entry_id: c.get("entryId")?.as_str()?.into(),
                    })
                })()
            } else {
                None
            };
            evidence
                .filter(|e| e.valid_for(request))
                .map(Status::Completed)
                .unwrap_or(Status::Unknown)
        }
        _ => Status::Unknown,
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Requested,
    Accepted,
    /// Typed into the pane; waiting for transcript evidence (Claude).
    Sent,
    Running,
    Completed(Evidence),
    Refused(String),
    Failed(String),
    Cancelled,
    Unknown,
}
impl Status {
    pub fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed(_) | Self::Refused(_) | Self::Failed(_) | Self::Cancelled
        )
    }
    pub fn text(&self) -> String {
        match self {
            Self::Requested => "Requested — completion not confirmed".into(),
            Self::Accepted => "Accepted — completion not confirmed".into(),
            Self::Running => "Running — completion not confirmed".into(),
            Self::Sent => {
                "Sent to the pane — waiting for evidence, completion not confirmed".into()
            }
            Self::Completed(Evidence::Persisted { path, size, .. }) => {
                format!("Saved: {path} ({size} bytes)")
            }
            Self::Completed(Evidence::Compacted { .. }) => "Native compact persisted".into(),
            Self::Completed(Evidence::Picked {
                new_session_id,
                handoff_path,
                ..
            }) => format!("New session {new_session_id} loaded the handoff {handoff_path}"),
            Self::Completed(Evidence::Handoff { path, .. }) => format!("Saved: {path}"),
            Self::Completed(Evidence::Compact { entry_id, .. }) => {
                format!("Native compact persisted: {entry_id}")
            }
            Self::Refused(why) => format!("Refused: {why}"),
            Self::Failed(why) => format!("Failed: {why}"),
            Self::Cancelled => "Cancelled — already persisted effects are not undone".into(),
            Self::Unknown => "Result unknown — query this operation; do not send it again".into(),
        }
    }
}

#[derive(Default)]
pub struct Controller {
    selection: Option<Session>,
    generation: u64,
    operation: Option<(Request, Status)>,
    used_ids: std::collections::HashSet<String>,
    cancel_requested: bool,
}
impl Controller {
    /// Selection invalidates confirmation but never implicitly cancels a request.
    pub fn select(&mut self, session: Option<Session>) {
        if self.selection != session {
            self.selection = session;
            self.generation += 1;
        }
    }
    pub fn disabled_reason(&self, action: Action) -> Option<&str> {
        if self.operation.as_ref().is_some_and(|(_, s)| !s.terminal()) {
            return Some("An operation is unresolved; query it before starting another");
        }
        match &self.selection {
            Some(s) => s.disabled_reason(action),
            None => Some("Select a session"),
        }
    }
    pub fn prepare(&self, action: Action) -> Result<Confirmation, &str> {
        if let Some(why) = self.disabled_reason(action) {
            return Err(why);
        }
        Ok(Confirmation {
            session: self.selection.clone().unwrap(),
            action,
            generation: self.generation,
        })
    }
    /// `current` must be freshly obtained by the adapter at confirmation time.
    /// The returned request is the only input for the supplied dispatch callback.
    pub fn confirm(
        &mut self,
        prompt: Confirmation,
        current: &Session,
        operation_id: String,
    ) -> Result<Request, &'static str> {
        if prompt.generation != self.generation
            || self.selection.as_ref() != Some(current)
            || &prompt.session != current
        {
            return Err("Session changed; review a new confirmation");
        }
        if self.disabled_reason(prompt.action).is_some() {
            return Err("Action is unavailable");
        }
        if operation_id.is_empty() || !self.used_ids.insert(operation_id.clone()) {
            return Err("Operation id must be new and nonempty");
        }
        let request = Request {
            operation_id,
            session: current.clone(),
            action: prompt.action,
        };
        self.operation = Some((request.clone(), Status::Requested));
        self.cancel_requested = false;
        self.generation += 1;
        Ok(request)
    }
    pub fn update(&mut self, operation_id: &str, identity: &str, status: Status) -> bool {
        let Some((request, previous)) = self.operation.as_mut() else {
            return false;
        };
        if request.operation_id != operation_id
            || request.session.identity != identity
            || previous.terminal()
        {
            return false;
        }
        // Do not regress to accepted/requested on an out-of-order acknowledgement.
        if matches!(status, Status::Requested)
            || matches!(
                (&*previous, &status),
                (
                    Status::Running | Status::Sent | Status::Unknown,
                    Status::Accepted
                ) | (Status::Unknown, Status::Sent)
            )
        {
            return false;
        }
        if let Status::Completed(evidence) = &status
            && !evidence.valid_for(request)
        {
            *previous = Status::Unknown;
            return false;
        }
        *previous = status;
        true
    }
    pub fn timeout_after_send(&mut self) {
        if let Some((_, status)) = &mut self.operation
            && !status.terminal()
        {
            *status = Status::Unknown;
        }
    }
    /// Call the adapter's query callback with this SAME request/id. Never dispatch again.
    pub fn query(&self) -> Option<&Request> {
        self.operation
            .as_ref()
            .filter(|(_, s)| !s.terminal())
            .map(|(r, _)| r)
    }
    /// A cancellation request is not a cancellation acknowledgement or rollback.
    pub fn request_cancel(&mut self) -> Option<Request> {
        let request = self.query()?.clone();
        if !request.session.cancel_supported || self.cancel_requested {
            return None;
        }
        self.cancel_requested = true;
        Some(request)
    }
    /// Results from another selection remain tracked but never appear on this session.
    pub fn visible_status(&self) -> Option<&Status> {
        let (request, status) = self.operation.as_ref()?;
        let selected = self.selection.as_ref()?;
        (selected.identity == request.session.identity).then_some(status)
    }
}

/// Off-thread discovery + authentication of one sampled session.
/// A seam so tests count calls without touching a real runtime directory.
pub type Authenticate = fn(&agentcenter_core::Session) -> Result<(Backend, Session), &'static str>;

/// Authenticated channel to one native session. Used only off the GTK thread.
#[derive(Debug, Clone)]
pub enum Backend {
    Pi(Box<agentcenter_core::control::pi::Endpoint>),
    /// Claude Code in a herdr pane: the pinned herdr server (CC4).
    Claude(agentcenter_core::control::herdr::Endpoint),
}

pub use agentcenter_core::control::pickup::{Progress, Stage};

/// Composite handoff-pickup adapter: CC5 (Pi inside a herdr pane) and CC6
/// (Claude Code in herdr) implement it. Called only inside the serialized
/// blocking worker, never on the GTK thread. `start` sends the effect once;
/// `status` reads the SAME operation ID and never sends again. Stages advance
/// strictly in order; a failed or unknown pickup reports where it stopped and
/// the old session (control/pickup.rs).
pub trait PickupPort: Send {
    fn start(&mut self, operation_id: &str) -> Result<Progress, &'static str>;
    fn status(&mut self, operation_id: &str) -> Result<Progress, &'static str>;
}

/// Builds the adapter for one confirmed pickup request. Build only, no I/O:
/// it runs on the GTK thread; sockets and transcripts belong in start/status.
///
/// Integration point for CC5/CC6: replace [`pickup_not_integrated`] in
/// `SessionControls::new` with the real adapters and stop setting
/// [`reason::PICKUP_NOT_INTEGRATED`] in `authenticated_session`/`claude_session`.
pub type PickupConnect =
    fn(&Backend, &agentcenter_core::Session, &Request) -> Result<Box<dyn PickupPort>, &'static str>;

/// Production default until CC5/CC6 are integrated: never reached, because
/// discovery hides the pickup button with `pickup_not_integrated`.
pub fn pickup_not_integrated(
    _: &Backend,
    _: &agentcenter_core::Session,
    _: &Request,
) -> Result<Box<dyn PickupPort>, &'static str> {
    Err("Pickup adapter is not integrated")
}

/// The six stages in their only order.
pub const STAGES: [Stage; 6] = [
    Stage::HandoffRunning,
    Stage::HandoffPersisted,
    Stage::Clearing,
    Stage::NewSessionConfirmed,
    Stage::PickupSent,
    Stage::PickupLoaded,
];
fn stage_index(stage: Stage) -> usize {
    STAGES.iter().position(|s| *s == stage).unwrap_or(0)
}
pub fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::HandoffRunning => "Writing the handoff",
        Stage::HandoffPersisted => "Handoff saved",
        Stage::Clearing => "Clearing the pane",
        Stage::NewSessionConfirmed => "New session confirmed",
        Stage::PickupSent => "Pickup sent",
        Stage::PickupLoaded => "Handoff loaded",
    }
}

/// Common pickup progress as UI status. Success needs pickup_loaded with a
/// new session ID and a persisted handoff; anything else is not completion.
pub fn status_from_progress(request: &Request, p: &Progress) -> Status {
    use agentcenter_core::control::State;
    if request.action != Action::Pickup
        || p.operation_id != request.operation_id
        || request.session.expected["sessionId"].as_str() != Some(p.old_session.session_id.as_str())
    {
        return Status::Unknown;
    }
    match p.state {
        State::Accepted => Status::Accepted,
        State::Running => Status::Running,
        State::Rejected => Status::Refused(p.code.into()),
        State::Failed => Status::Failed(p.code.into()),
        State::Cancelled => Status::Cancelled,
        State::Unknown => Status::Unknown,
        State::Completed => match (&p.handoff, &p.new_session_id) {
            (Some(a), Some(id)) if p.stage == Stage::PickupLoaded => {
                let e = Evidence::Picked {
                    new_session_id: id.clone(),
                    handoff_path: a.path.to_string_lossy().into_owned(),
                    sha256: a.sha256.clone(),
                };
                if e.valid_for(request) {
                    Status::Completed(e)
                } else {
                    Status::Unknown
                }
            }
            _ => Status::Unknown,
        },
    }
}

/// GTK owns presentation only; all discovery and socket work runs off-thread.
pub struct SessionControls {
    pub widget: gtk::Box,
    panel: Rc<Panel>,
}
struct Panel {
    widget: gtk::Box,
    live: bool,
    reason: gtk::Label,
    buttons: Vec<(Action, gtk::Button)>,
    query: gtk::Button,
    cancel: gtk::Button,
    /// Last herdr pane status seen for the selection; a change rediscovers.
    activity: RefCell<Option<String>>,
    /// The one line that explains a failed discovery or dispatch for this
    /// selection. Only `render` writes the status line; callers set this.
    notice: RefCell<Option<String>>,
    query_pending: Cell<bool>,
    completed_at: RefCell<Option<String>>,
    refresh_pending: Cell<bool>,
    cancel_pending: Cell<bool>,
    controller: RefCell<Controller>,
    native: RefCell<Option<agentcenter_core::Session>>,
    operation: RefCell<Option<TransportOperation>>,
    busy: Cell<bool>,
    /// The window whose activation rediscovers, with its handler: one at a
    /// time, dropped on unrealize (re-parenting must not stack handlers).
    active: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
    /// `busy` is a capability read (refresh), not an effect in flight.
    discovering: Cell<bool>,
    /// A confirmation accepted while a rediscovery was in flight; it is
    /// dispatched (and revalidated) as soon as that read returns.
    parked: RefCell<Option<Confirmation>>,
    epoch: Cell<u64>,
    endpoint_key: RefCell<Option<String>>,
    authenticate: Cell<Authenticate>,
    /// Polling cadence while an operation is accepted/running/sent.
    poll: Cell<std::time::Duration>,
    pickup_connect: Cell<PickupConnect>,
    /// Last pickup progress, for its own operation ID only.
    progress: RefCell<Option<Progress>>,
    stages: StageList,
    /// What was last announced to the screen reader (op, stage, status).
    announced: RefCell<Option<String>>,
    /// Announcements in order (bounded), read by the GTK tests.
    announcements: RefCell<Vec<String>>,
}
/// The six pickup stages, one row each: number, name, state in words.
struct StageList {
    widget: gtk::Box,
    title: gtk::Label,
    rows: Vec<StageRow>,
}
struct StageRow {
    widget: gtk::Box,
    state: gtk::Label,
    check: gtk::Image,
}
impl StageList {
    fn new() -> Self {
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .accessible_role(gtk::AccessibleRole::List)
            .visible(false)
            .build();
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.add_css_class("detail-caption");
        title.add_css_class("numeric");
        widget.append(&title);
        widget.update_relation(&[gtk::accessible::Relation::LabelledBy(&[title.upcast_ref()])]);
        let rows = STAGES
            .iter()
            .enumerate()
            .map(|(i, stage)| {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(8)
                    .accessible_role(gtk::AccessibleRole::ListItem)
                    .build();
                let number = gtk::Label::new(Some(&(i + 1).to_string()));
                number.set_width_chars(2);
                number.set_xalign(1.0);
                number.add_css_class("numeric");
                number.add_css_class("dim-label");
                let name = gtk::Label::new(Some(stage_name(*stage)));
                name.set_xalign(0.0);
                name.set_hexpand(true);
                name.set_wrap(true);
                name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                let check = gtk::Image::from_icon_name("object-select-symbolic");
                check.set_accessible_role(gtk::AccessibleRole::Presentation);
                check.set_visible(false);
                let state = gtk::Label::new(None);
                state.set_valign(gtk::Align::Center);
                for w in [
                    number.upcast_ref::<gtk::Widget>(),
                    name.upcast_ref(),
                    check.upcast_ref(),
                    state.upcast_ref(),
                ] {
                    row.append(w);
                }
                widget.append(&row);
                StageRow {
                    widget: row,
                    state,
                    check,
                }
            })
            .collect();
        Self {
            widget,
            title,
            rows,
        }
    }
}
/// What a stage row says, in words (colour only repeats it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageMark {
    Done,
    /// The current stage of an operation still in flight: amber.
    Now,
    /// Where a failed, refused or cancelled pickup stopped: crimson.
    Stopped,
    /// Where an unknown pickup was last seen: crimson.
    Unknown,
    /// pickup_loaded with evidence: the only success mark (check icon).
    Loaded,
    Pending,
    NotReached,
}
impl StageMark {
    fn text(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Now => "NOW",
            Self::Stopped => "STOPPED",
            Self::Unknown => "UNKNOWN",
            Self::Loaded => "loaded",
            Self::Pending => "pending",
            Self::NotReached => "not reached",
        }
    }
}
/// The mark of every stage for one progress report and its UI status.
pub fn stage_marks(progress: &Progress, status: &Status) -> [StageMark; 6] {
    let at = stage_index(progress.stage);
    let loaded = matches!(status, Status::Completed(Evidence::Picked { .. }))
        && progress.stage == Stage::PickupLoaded
        && progress
            .new_session_id
            .as_deref()
            .is_some_and(|s| !s.is_empty());
    let stopped = match status {
        Status::Unknown => Some(StageMark::Unknown),
        Status::Failed(_) | Status::Refused(_) | Status::Cancelled => Some(StageMark::Stopped),
        // Completed without pickup evidence is never success.
        Status::Completed(_) if !loaded => Some(StageMark::Unknown),
        _ => None,
    };
    std::array::from_fn(|i| match i.cmp(&at) {
        std::cmp::Ordering::Less => StageMark::Done,
        std::cmp::Ordering::Equal if loaded => StageMark::Loaded,
        std::cmp::Ordering::Equal => stopped.unwrap_or(StageMark::Now),
        std::cmp::Ordering::Greater if stopped.is_some() => StageMark::NotReached,
        std::cmp::Ordering::Greater => StageMark::Pending,
    })
}
impl SessionControls {
    pub fn new(live: bool) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        // The section holds focus only when nothing in it can (keep_focus);
        // once focus moves on it is no Tab stop again.
        widget.update_property(&[gtk::accessible::Property::Label("Session controls")]);
        let held = gtk::EventControllerFocus::new();
        held.connect_is_focus_notify(|c| {
            if !c.is_focus()
                && let Some(w) = c.widget()
            {
                w.set_focusable(false);
            }
        });
        widget.add_controller(held);
        let buttons = [Action::Handoff, Action::Compact, Action::Pickup]
            .into_iter()
            .map(|a| {
                let b = gtk::Button::with_label(&format!("{}…", a.label()));
                b.set_height_request(44);
                widget.append(&b);
                (a, b)
            })
            .collect();
        let stages = StageList::new();
        widget.append(&stages.widget);
        let reason = text("Select a session");
        // Tab into the selectable status line selects it all (copy the path);
        // the highlight leaves with the focus instead of lingering.
        let read = gtk::EventControllerFocus::new();
        read.connect_leave(|c| {
            if let Some(l) = c.widget().and_downcast::<gtk::Label>() {
                l.select_region(0, 0);
            }
        });
        reason.add_controller(read);
        widget.append(&reason);
        let query = gtk::Button::with_label("Query operation");
        let cancel = gtk::Button::with_label("Request cancellation");
        for b in [&query, &cancel] {
            b.set_height_request(44);
            widget.append(b);
        }
        let panel = Rc::new(Panel {
            widget: widget.clone(),
            live,
            reason,
            buttons,
            query,
            cancel,
            activity: RefCell::new(None),
            notice: RefCell::new(None),
            query_pending: Cell::new(false),
            completed_at: RefCell::new(None),
            refresh_pending: Cell::new(false),
            cancel_pending: Cell::new(false),
            controller: RefCell::new(Controller::default()),
            native: RefCell::new(None),
            operation: RefCell::new(None),
            busy: Cell::new(false),
            active: RefCell::new(None),
            discovering: Cell::new(false),
            parked: RefCell::new(None),
            epoch: Cell::new(0),
            endpoint_key: RefCell::new(None),
            authenticate: Cell::new(authenticated_session),
            poll: Cell::new(std::time::Duration::from_millis(300)),
            pickup_connect: Cell::new(pickup_not_integrated),
            progress: RefCell::new(None),
            stages,
            announced: RefCell::new(None),
            announcements: RefCell::new(Vec::new()),
        });
        for (action, b) in &panel.buttons {
            let weak = Rc::downgrade(&panel);
            let a = *action;
            b.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    p.prepare(a);
                }
            });
        }
        for (b, action) in [
            (&panel.query, agentcenter_core::control::pi::Action::Status),
            (&panel.cancel, agentcenter_core::control::pi::Action::Cancel),
        ] {
            let weak = Rc::downgrade(&panel);
            b.connect_clicked(move |_| {
                if let Some(p) = weak.upgrade() {
                    if action == agentcenter_core::control::pi::Action::Cancel {
                        p.request_cancellation();
                    } else if p.busy.get() {
                        // Never a dead click: run right after the read in flight.
                        p.query_pending.set(true);
                    } else {
                        p.reconcile(action);
                    }
                }
            });
        }
        // No refresh button: capabilities are rediscovered off-thread when the
        // window regains focus (and on selection, see set_session).
        let weak = Rc::downgrade(&panel);
        widget.connect_realize(move |w| {
            let Some(p) = weak.upgrade() else { return };
            p.forget_window();
            let Some(window) = w.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let weak = weak.clone();
            let id = window.connect_is_active_notify(move |window| {
                if window.is_active()
                    && let Some(p) = weak.upgrade()
                {
                    p.rediscover();
                }
            });
            p.active.replace(Some((window.downgrade(), id)));
        });
        let weak = Rc::downgrade(&panel);
        widget.connect_unrealize(move |_| {
            if let Some(p) = weak.upgrade() {
                p.forget_window();
            }
        });
        panel.render();
        Self { widget, panel }
    }
    pub fn set_session(&self, session: Option<&agentcenter_core::Session>) {
        let p = &self.panel;
        let key = |s: &agentcenter_core::Session| {
            (
                s.pid,
                s.starttime,
                s.agent.clone(),
                s.overlay.as_ref().and_then(|o| o.agent_session_id.clone()),
            )
        };
        let activity = session.and_then(|s| s.overlay.as_ref()?.herdr.as_ref()?.status.clone());
        if p.native.borrow().as_ref().map(key) == session.map(key) {
            // Same session: only a herdr status change (busy -> idle) may
            // change capability. One rediscovery per change, never per tick;
            // it reads this sample, not the one from the first selection.
            if p.activity.replace(activity.clone()) != activity {
                *p.native.borrow_mut() = session.cloned();
                p.rediscover();
            }
            return;
        }
        p.activity.replace(activity);
        *p.native.borrow_mut() = session.cloned();
        p.epoch.set(p.epoch.get().wrapping_add(1));
        p.controller.borrow_mut().select(None);
        *p.endpoint_key.borrow_mut() = None;
        p.notice.replace(None);
        p.parked.replace(None);
        let controllable = selection_blocker(p.live, session).is_empty();
        p.refresh_pending.set(controllable);
        // Render shows the blocker itself: the line and its visibility are
        // decided together, never written after render (verifier B1).
        p.render();
        if controllable {
            p.refresh();
        }
    }
    /// Fixture/test seam: replaces discovery (synthetic sessions only).
    #[allow(dead_code)] // used by examples/pi_controls_fixture.rs and tests
    pub fn set_discovery(&self, discover: Authenticate) {
        self.panel.authenticate.set(discover);
    }
    /// Pickup adapter seam: fakes in tests/fixtures, CC5/CC6 when integrated.
    #[allow(dead_code)] // used by examples/pi_controls_fixture.rs and tests
    pub fn set_pickup(&self, connect: PickupConnect) {
        self.panel.pickup_connect.set(connect);
    }
    /// Polling cadence while an operation is in flight (fixtures/tests).
    #[allow(dead_code)]
    pub fn set_poll(&self, every: std::time::Duration) {
        self.panel.poll.set(every);
    }
    /// Screen-reader announcements made so far, oldest first (bounded).
    #[allow(dead_code)]
    pub fn announcements(&self) -> Vec<String> {
        self.panel.announcements.borrow().clone()
    }
    /// The stage marks currently rendered, if the pickup list is shown.
    #[allow(dead_code)]
    pub fn stage_marks(&self) -> Option<[StageMark; 6]> {
        let p = &self.panel;
        p.stages.widget.is_visible().then(|| {
            let (progress, status, _) = p.visible_pickup()?;
            Some(stage_marks(&progress, &status))
        })?
    }
}
impl Panel {
    fn forget_window(&self) {
        if let Some((window, id)) = self.active.take()
            && let Some(window) = window.upgrade()
        {
            window.disconnect(id);
        }
    }
    /// Rediscovery outside a selection change (focus regained, pane status
    /// changed). Skipped while an operation is unresolved: it keeps its own
    /// request and the panel shows its state instead of the actions.
    fn rediscover(self: &Rc<Self>) {
        if self.controller.borrow().query().is_none()
            && selection_blocker(self.live, self.native.borrow().as_ref()).is_empty()
        {
            self.refresh();
        }
    }
    fn render(&self) {
        if self.focus().as_ref() != Some(self.widget.upcast_ref()) {
            self.widget.set_focusable(false);
        }
        let held = self.focus().is_some();
        self.render_controls();
        // Whatever leaves while focused (an action, Query/Cancel, the status
        // line) must not take the focus with it: GTK leaves it on a hidden
        // label, and drops it (None) when a button turns insensitive. The
        // section itself only holds focus until a control comes back.
        let lost = match self.window_focus() {
            None => held,
            Some(f) => {
                self.contains(&f)
                    && (!self.shows(&f) || f == *self.widget.upcast_ref::<gtk::Widget>())
            }
        };
        if lost {
            self.keep_focus();
        }
    }
    fn window_focus(&self) -> Option<gtk::Widget> {
        let window = self.widget.root().and_downcast::<gtk::Window>()?;
        gtk::prelude::GtkWindowExt::focus(&window)
    }
    fn contains(&self, w: &gtk::Widget) -> bool {
        w == self.widget.upcast_ref::<gtk::Widget>() || w.is_ancestor(&self.widget)
    }
    /// The window's focus widget, when it is in this section.
    fn focus(&self) -> Option<gtk::Widget> {
        self.window_focus().filter(|f| self.contains(f))
    }
    /// `w` and every parent up to the section are visible.
    fn shows(&self, w: &gtk::Widget) -> bool {
        let top: &gtk::Widget = self.widget.upcast_ref();
        let mut at = Some(w.clone());
        while let Some(x) = at {
            if !x.is_visible() {
                return false;
            }
            if &x == top {
                return true;
            }
            at = x.parent();
        }
        false
    }
    /// Moves focus to the first control still shown, else to the status
    /// line, else to the section itself (a Tab stop only while it holds it).
    fn keep_focus(&self) {
        let target = [&self.query, &self.cancel]
            .into_iter()
            .chain(self.buttons.iter().map(|(_, b)| b))
            .map(|b| b.upcast_ref::<gtk::Widget>())
            .chain([self.reason.upcast_ref()])
            .find(|w| self.shows(w) && w.is_sensitive() && w.is_focusable())
            .cloned();
        match target {
            Some(w) => {
                w.grab_focus();
                // A selectable label selects all its text on focus; the
                // status line is read, not highlighted.
                if w == *self.reason.upcast_ref::<gtk::Widget>() {
                    self.reason.select_region(0, 0);
                }
            }
            None => {
                self.widget.set_focusable(true);
                self.widget.grab_focus();
            }
        }
    }
    fn render_controls(&self) {
        let c = self.controller.borrow();
        // Rediscovering what is already on screen (window focus back, herdr
        // status change) keeps it in place: no layout jump, focus stays on
        // its control. Only the first read of a selection says "Checking".
        let settled = c.selection.is_some() || self.notice.borrow().is_some();
        let busy = self.busy.get() && !(self.discovering.get() && settled);
        // Owner rule (22/10): a control the session cannot run is not shown,
        // not even disabled. While an operation is open, the actions give way
        // to its state; Query/Cancel only while they can do something.
        let mut shown = 0;
        for (a, b) in &self.buttons {
            let visible = !busy && c.disabled_reason(*a).is_none();
            b.set_visible(visible);
            b.set_sensitive(visible);
            shown += usize::from(visible);
        }
        let open = c.query();
        let status = c.operation.as_ref().map(|(_, s)| s);
        let queryable = open.is_some()
            && matches!(
                status,
                Some(Status::Unknown | Status::Accepted | Status::Sent)
            );
        self.query.set_visible(queryable);
        self.query.set_sensitive(queryable);
        // The pickup contract has no cancel: never offer one.
        let cancellable = !c.cancel_requested
            && open.is_some_and(|r| r.session.cancel_supported && r.action != Action::Pickup);
        self.cancel.set_visible(cancellable);
        self.cancel.set_sensitive(cancellable);
        if let Some(request) = open {
            self.query
                .set_label(&format!("Query operation for {}", request.session.name));
            self.cancel.set_label(&format!(
                "Request cancellation for {}",
                request.session.name
            ));
        }
        let mut lines = Vec::new();
        let pickup = self.visible_pickup();
        self.render_stages(pickup.as_ref());
        if let Some((progress, status, session)) = &pickup {
            lines.extend(pickup_lines(progress, status, session));
        } else if let Some(s) = c.visible_status() {
            lines.push(s.text());
            if matches!(s, Status::Completed(_))
                && let Some(date) = self.completed_at.borrow().as_ref()
            {
                lines.push(format!("Persisted at: {date}"));
            }
            if c.cancel_requested && open.is_some() {
                lines.push("Cancellation requested; waiting for acknowledgement.".into());
            }
        } else if let Some(request) = open {
            lines.push(format!(
                "Unresolved operation for {}. Query it before starting another.",
                request.session.name
            ));
        } else if busy {
            lines.push("Checking what this session supports…".into());
        } else if let Some(notice) = self.notice.borrow().as_ref() {
            lines.push(notice.clone());
        } else if let Some(sel) = &c.selection {
            // At most ONE line of reason, never a tooltip-only explanation.
            if let Some(line) = reason_line(sel, shown) {
                lines.push(line);
            }
        } else {
            let why = selection_blocker(self.live, self.native.borrow().as_ref());
            if !why.is_empty() {
                lines.push(why.into());
            }
        }
        self.reason.set_text(&lines.join("\n"));
        self.reason.set_visible(!lines.is_empty());
        // Handoff and Compact are announced like the pickup: once per status
        // of the operation shown on this selection (Requested is the click).
        let plain = c
            .operation
            .as_ref()
            .filter(|(r, s)| {
                r.action != Action::Pickup
                    && *s != Status::Requested
                    && c.visible_status().is_some()
            })
            .map(|(r, s)| (r.operation_id.clone(), r.action, s.clone()));
        drop(c);
        if let Some((progress, status, _)) = &pickup {
            self.announce_stage(progress, status, lines.first().map_or("", String::as_str));
        } else if let Some((id, action, status)) = plain {
            self.announce(
                format!("{id}|-|{}", status.text()),
                format!("{}: {}", action.label(), status.text()),
                &status,
            );
        }
    }
    /// The pickup this selection may see: its own request, or the new session
    /// it produced (same pane, new native ID after pickup_loaded).
    fn visible_pickup(&self) -> Option<(Progress, Status, Session)> {
        let c = self.controller.borrow();
        let (request, status) = c.operation.as_ref()?;
        let progress = self.progress.borrow().clone()?;
        if request.action != Action::Pickup || progress.operation_id != request.operation_id {
            return None;
        }
        let own = c
            .selection
            .as_ref()
            .is_some_and(|s| s.identity == request.session.identity);
        let successor = matches!(status, Status::Completed(Evidence::Picked { .. }))
            && progress.new_session_id.is_some()
            && self
                .native
                .borrow()
                .as_ref()
                .and_then(|n| n.overlay.as_ref()?.agent_session_id.clone())
                == progress.new_session_id;
        (own || successor).then(|| (progress, status.clone(), request.session.clone()))
    }
    fn render_stages(&self, pickup: Option<&(Progress, Status, Session)>) {
        let list = &self.stages;
        let Some((progress, status, _)) = pickup else {
            list.widget.set_visible(false);
            return;
        };
        list.widget.set_visible(true);
        let at = stage_index(progress.stage);
        list.title.set_text(&format!(
            "{} · stage {} of {}",
            Action::Pickup.label(),
            at + 1,
            STAGES.len()
        ));
        for ((row, mark), stage) in list
            .rows
            .iter()
            .zip(stage_marks(progress, status))
            .zip(STAGES)
        {
            row.state.set_text(mark.text());
            for class in ["ac-pill", "busy", "blocked", "dim-label"] {
                row.state.remove_css_class(class);
            }
            match mark {
                StageMark::Now => {
                    row.state.add_css_class("ac-pill");
                    row.state.add_css_class("busy");
                }
                StageMark::Stopped | StageMark::Unknown => {
                    row.state.add_css_class("ac-pill");
                    row.state.add_css_class("blocked");
                }
                StageMark::Loaded => {}
                _ => row.state.add_css_class("dim-label"),
            }
            row.check.set_visible(mark == StageMark::Loaded);
            row.widget
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "Stage {} of {}, {}: {}",
                    stage_index(stage) + 1,
                    STAGES.len(),
                    stage_name(stage),
                    mark.text().to_lowercase()
                ))]);
        }
    }
    /// One announcement per stage or status change, never per render.
    fn announce_stage(&self, progress: &Progress, status: &Status, line: &str) {
        let key = format!(
            "{}|{:?}|{}",
            progress.operation_id,
            progress.stage,
            status.text()
        );
        self.announce(key, format!("{}: {line}", Action::Pickup.label()), status);
    }
    /// Announces `message` unless `key` was the last announcement.
    fn announce(&self, key: String, message: String, status: &Status) {
        if self.announced.borrow().as_deref() == Some(key.as_str()) {
            return;
        }
        self.announced.replace(Some(key));
        let priority = if matches!(
            status,
            Status::Completed(_) | Status::Accepted | Status::Running
        ) {
            gtk::AccessibleAnnouncementPriority::Medium
        } else {
            gtk::AccessibleAnnouncementPriority::High
        };
        // GtkTestATContext (no a11y bus: Broadway, GTK tests) has no announce
        // vfunc and GTK 4.22 calls it anyway (SIGSEGV at address 0). Only the
        // backends that implement it get the call; the log keeps every line.
        let ctx = self.widget.at_context().type_().name();
        if matches!(ctx, "GtkAtSpiContext" | "GtkAccessKitContext") {
            self.widget.announce(&message, priority);
        }
        let mut log = self.announcements.borrow_mut();
        if log.len() >= 32 {
            log.remove(0);
        }
        log.push(message);
    }
    /// Records a progress report for the open pickup. A stage never goes
    /// back; a report for another operation is ignored.
    fn apply_progress(&self, request: &Request, progress: Option<Progress>) {
        let Some(progress) = progress else {
            return;
        };
        if request.action != Action::Pickup || progress.operation_id != request.operation_id {
            return;
        }
        let mut slot = self.progress.borrow_mut();
        let regress = slot.as_ref().is_some_and(|old| {
            old.operation_id == progress.operation_id && progress.stage < old.stage
        });
        if !regress {
            *slot = Some(progress);
        }
    }
    fn refresh(self: &Rc<Self>) {
        if !selection_blocker(self.live, self.native.borrow().as_ref()).is_empty() {
            return;
        }
        if self.busy.replace(true) {
            self.refresh_pending.set(true);
            return;
        }
        self.refresh_pending.set(false);
        let Some(native) = self.native.borrow().clone() else {
            self.busy.set(false);
            return;
        };
        self.discovering.set(true);
        self.render();
        let epoch = self.epoch.get();
        let authenticate = self.authenticate.get();
        let weak = Rc::downgrade(self);
        let claude = native.agent == "claude";
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || authenticate(&native)).await;
            let Some(p) = weak.upgrade() else {
                return;
            };
            p.busy.set(false);
            p.discovering.set(false);
            if epoch != p.epoch.get() {
                p.refresh();
                return;
            }
            match result {
                Ok(Ok((endpoint, s))) => {
                    *p.endpoint_key.borrow_mut() = endpoint_key(&endpoint);
                    p.notice.replace(None);
                    p.controller.borrow_mut().select(Some(s));
                    p.render();
                }
                failure => {
                    p.controller.borrow_mut().select(None);
                    // One line; capabilities are checked again on focus.
                    p.notice.replace(Some(match failure {
                        Ok(Err(code)) if claude => humanize(Agent::Claude, code),
                        _ if claude => "Claude Code identity could not be checked in herdr.".into(),
                        _ => "AgentCenter extension not found in this Pi session; enable it in Pi."
                            .into(),
                    }));
                    p.render();
                }
            }
            if let Some(prompt) = p.parked.take() {
                p.dispatch(prompt);
            } else if p.refresh_pending.get() {
                p.refresh();
            }
        });
    }
    fn prepare(self: &Rc<Self>, action: Action) {
        // A rediscovery keeps the controls on screen, so they stay live: the
        // confirmation is revalidated at dispatch anyway.
        if self.busy.get() && !self.discovering.get() {
            return;
        }
        let Ok(prompt) = self.controller.borrow().prepare(action) else {
            return;
        };
        let d = confirmation_dialog(&prompt);
        let weak = Rc::downgrade(self);
        let slot = RefCell::new(Some(prompt));
        d.connect_response(None, move |_, id| {
            let Some(prompt) = slot.borrow_mut().take() else {
                return;
            };
            if id == "confirm"
                && let Some(p) = weak.upgrade()
            {
                p.dispatch(prompt);
            }
        });
        // The dialog returns focus to the button that opened it, which may be
        // gone by then (the operation started): keep it in the section, or
        // the next dialog in this window is presented without content.
        let weak = Rc::downgrade(self);
        d.connect_closed(move |_| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                let Some(p) = weak.upgrade() else { return };
                let lost = p
                    .widget
                    .root()
                    .and_downcast::<gtk::Window>()
                    .is_none_or(|w| {
                        gtk::prelude::GtkWindowExt::focus(&w)
                            .is_none_or(|f| !f.is_visible() || !f.is_drawable())
                    });
                if lost {
                    p.keep_focus();
                }
            });
        });
        d.present(Some(&self.widget));
    }
    fn dispatch(self: &Rc<Self>, prompt: Confirmation) {
        if self.busy.get() {
            if self.discovering.get() {
                self.parked.replace(Some(prompt));
            }
            return;
        }
        self.busy.set(true);
        let Some(native) = self.native.borrow().clone() else {
            self.busy.set(false);
            return;
        };
        let epoch = self.epoch.get();
        let authenticate = self.authenticate.get();
        let weak = Rc::downgrade(self);
        self.notice.replace(None);
        self.render();
        let sampled = native.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || authenticate(&native)).await;
            let Some(p) = weak.upgrade() else {
                return;
            };
            p.busy.set(false);
            if epoch != p.epoch.get() {
                p.render();
                p.refresh();
                return;
            }
            let Ok(Ok((endpoint, current))) = result else {
                p.controller.borrow_mut().select(None);
                p.notice.replace(Some(
                    "Identity could not be revalidated; nothing was sent.".into(),
                ));
                p.render();
                return;
            };
            if endpoint_key(&endpoint).as_ref() != p.endpoint_key.borrow().as_ref() {
                *p.endpoint_key.borrow_mut() = endpoint_key(&endpoint);
                p.controller.borrow_mut().select(Some(current));
                p.notice.replace(Some(
                    "Endpoint or metadata changed; review a new confirmation.".into(),
                ));
                p.render();
                return;
            }
            let request = p.controller.borrow_mut().confirm(
                prompt,
                &current,
                glib::uuid_string_random().to_string(),
            );
            match request {
                Ok(r) => {
                    p.completed_at.replace(None);
                    let connect = p.pickup_connect.get();
                    match TransportOperation::new(endpoint, &sampled, r, connect) {
                        Ok(op) => {
                            *p.operation.borrow_mut() = Some(op);
                            p.reconcile_dispatch();
                        }
                        Err(why) => {
                            p.controller.borrow_mut().timeout_after_send();
                            p.notice.replace(Some(why.into()));
                            p.render();
                        }
                    }
                }
                Err(why) => {
                    // Updated metadata requires a fresh confirmation, not reselecting.
                    p.controller.borrow_mut().select(Some(current));
                    p.notice.replace(Some(why.into()));
                    p.render();
                }
            }
        });
    }
    fn request_cancellation(self: &Rc<Self>) {
        if self.controller.borrow_mut().request_cancel().is_none() {
            return;
        }
        self.cancel_pending.set(true);
        self.render();
        if !self.busy.get() {
            self.reconcile(agentcenter_core::control::pi::Action::Cancel);
        }
    }
    fn reconcile_dispatch(self: &Rc<Self>) {
        let action = self
            .operation
            .borrow()
            .as_ref()
            .map(TransportOperation::action);
        if let Some(a) = action {
            self.reconcile(a);
        }
    }
    fn reconcile(self: &Rc<Self>, action: agentcenter_core::control::pi::Action) {
        use agentcenter_core::control::pi;
        if self.busy.replace(true) {
            return;
        }
        if action == pi::Action::Cancel && !self.cancel_pending.replace(false) {
            self.busy.set(false);
            return;
        }
        let Some(mut op) = self.operation.borrow_mut().take() else {
            self.busy.set(false);
            return;
        };
        self.render();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || {
                let (status, date) = op.exchange(action);
                (op, status, date)
            })
            .await;
            let Some(p) = weak.upgrade() else {
                return;
            };
            match result {
                Ok((mut op, status, date)) => {
                    let poll = matches!(status, Status::Accepted | Status::Running | Status::Sent);
                    let completed = matches!(status, Status::Completed(_));
                    let terminal = status.terminal();
                    if terminal {
                        p.cancel_pending.set(false);
                    }
                    // Claude evidence is a transcript read: a slower cadence.
                    let delay = if op.is_claude() {
                        std::time::Duration::from_secs(2)
                    } else {
                        p.poll.get()
                    };
                    let updated = p.controller.borrow_mut().update(
                        &op.ui_request.operation_id,
                        &op.ui_request.session.identity,
                        status,
                    );
                    p.apply_progress(&op.ui_request, op.progress.take());
                    if updated && completed {
                        p.completed_at.replace(date);
                    }
                    // A finished operation changes revision/capability: the
                    // panel rediscovers by itself (no Refresh button).
                    if updated && terminal {
                        p.refresh_pending.set(true);
                    }
                    *p.operation.borrow_mut() = Some(op);
                    p.busy.set(poll);
                    p.render();
                    if poll {
                        let weak = Rc::downgrade(&p);
                        glib::timeout_add_local_once(delay, move || {
                            if let Some(p) = weak.upgrade() {
                                p.busy.set(false);
                                p.reconcile(if p.cancel_pending.get() {
                                    pi::Action::Cancel
                                } else {
                                    pi::Action::Status
                                });
                            }
                        });
                    } else if p.cancel_pending.get() {
                        p.reconcile(pi::Action::Cancel);
                    } else if p.query_pending.replace(false) {
                        p.reconcile(pi::Action::Status);
                    } else if p.refresh_pending.get() {
                        p.refresh();
                    }
                }
                Err(_) => {
                    p.controller.borrow_mut().timeout_after_send();
                    p.busy.set(false);
                    p.render();
                }
            }
        });
    }
}
/// The pickup's text under the stage list: where it is and, when it stopped
/// or is unknown, where it stopped and how to resume the old session.
fn pickup_lines(progress: &Progress, status: &Status, old: &Session) -> Vec<String> {
    let at = format!(
        "stage {} of {} ({})",
        stage_index(progress.stage) + 1,
        STAGES.len(),
        stage_name(progress.stage)
    );
    let keep = format!(
        "The old session is kept; to resume it, {}.",
        old.resume_route()
    );
    match status {
        Status::Completed(Evidence::Picked { .. }) if progress.stage == Stage::PickupLoaded => {
            vec![status.text()]
        }
        Status::Requested | Status::Accepted | Status::Running | Status::Sent => {
            vec![format!("Now at {at}; completion not confirmed.")]
        }
        Status::Unknown | Status::Completed(_) => vec![
            format!("Result unknown at {at}. Query this operation; do not send it again."),
            keep,
        ],
        Status::Failed(code) | Status::Refused(code) => {
            vec![
                format!("Stopped at {at}: {}.", code.replace('_', " ")),
                keep,
            ]
        }
        Status::Cancelled => vec![format!("Cancelled at {at}."), keep],
    }
}
fn agent_name(agent: Agent) -> &'static str {
    match agent {
        Agent::Pi => "Pi",
        Agent::Claude => "Claude Code",
    }
}
/// A closed capability code as one sentence. Unknown codes are shown as-is.
fn humanize(agent: Agent, code: &str) -> String {
    let who = agent_name(agent);
    match code {
        "busy" => format!("{who} is busy; controls return when it is idle."),
        "pending" => format!("{who} has an operation pending."),
        "vigia_swap" => format!("squad-vigia is swapping this {who} session."),
        "no_transcript" => format!("{who} transcript not found."),
        "model_auth_required" => format!("{who} model needs authentication."),
        reason::PI_OUTSIDE_HERDR => "Handoff + new session needs Pi inside a herdr pane.".into(),
        other => format!("{who} controls unavailable: {}.", other.replace('_', " ")),
    }
}
/// The panel's single reason line for what the selection cannot do.
fn reason_line(s: &Session, shown: usize) -> Option<String> {
    let first = [Action::Handoff, Action::Compact]
        .into_iter()
        .find_map(|a| s.disabled_reason(a).map(|why| (a, why)));
    match (shown, first) {
        (0, Some((_, why))) => Some(humanize(s.agent, why)),
        (_, Some((a, why))) => Some(format!(
            "{} unavailable: {}",
            a.label(),
            humanize(s.agent, why)
        )),
        (_, None) => s
            .disabled_reason(Action::Pickup)
            .filter(|why| *why != reason::PICKUP_NOT_INTEGRATED)
            .map(|why| humanize(s.agent, why)),
    }
}
/// Synchronous, sample-only gate: no /proc, socket or transcript access.
fn selection_blocker(live: bool, session: Option<&agentcenter_core::Session>) -> &'static str {
    if !live {
        return "Actions are off: recorded snapshot. Use the native agent manually.";
    }
    let Some(session) = session else {
        return "Select a session";
    };
    if session.agent == "claude" {
        let pane = agentcenter_core::control::claude::Subject::from_session(session);
        if session.starttime.is_none() || pane.is_err() {
            return "Claude Code runs outside herdr; control it in its own terminal.";
        }
        return "";
    }
    if session.agent != "pi" {
        return "This agent has no control channel in AgentCenter.";
    }
    if session.starttime.is_none()
        || session
            .overlay
            .as_ref()
            .and_then(|o| o.agent_session_id.as_deref())
            .is_none_or(str::is_empty)
    {
        return "Native process/session identity is unavailable. Use Pi manually.";
    }
    ""
}

/// Keep the authenticated channel and ORIGINAL request across every read.
struct TransportOperation {
    ui_request: Request,
    channel: Channel,
    sent: bool,
    /// Last pickup progress read by `exchange`, handed to the panel.
    progress: Option<Progress>,
}
enum Channel {
    Pi {
        endpoint: Box<agentcenter_core::control::pi::Endpoint>,
        original: Box<agentcenter_core::control::pi::Request>,
    },
    /// The same core controller answers start and every status of this ID.
    Claude {
        controller: Box<agentcenter_core::control::claude::Controller>,
        subject: agentcenter_core::control::claude::Subject,
    },
    /// Composite handoff-pickup (CC5/CC6) through [`PickupPort`].
    Pickup {
        port: Box<dyn PickupPort>,
        claude: bool,
    },
}
impl TransportOperation {
    fn new(
        backend: Backend,
        native: &agentcenter_core::Session,
        ui_request: Request,
        connect: PickupConnect,
    ) -> Result<Self, &'static str> {
        use agentcenter_core::control::{claude, pi};
        if ui_request.action == Action::Pickup {
            let port = connect(&backend, native, &ui_request)?;
            return Ok(Self {
                channel: Channel::Pickup {
                    port,
                    claude: ui_request.session.agent == Agent::Claude,
                },
                ui_request,
                sent: false,
                progress: None,
            });
        }
        let channel = match backend {
            Backend::Pi(endpoint) => {
                let action = match ui_request.action {
                    Action::Handoff => pi::Action::Handoff,
                    Action::Compact => pi::Action::Compact,
                    Action::Pickup => return Err("Pickup adapter is not integrated"),
                };
                if endpoint.handoff_directory.as_ref().and_then(|p| p.to_str())
                    != Some(ui_request.session.handoff_destination.as_str())
                {
                    return Err("Private handoff destination changed; review a new confirmation");
                }
                let original = endpoint
                    .request(ui_request.operation_id.clone(), action)
                    .map_err(|_| "Invalid endpoint")?;
                if serde_json::to_value(&original.expected).ok().as_ref()
                    != Some(&ui_request.session.expected)
                {
                    return Err("Session changed; review a new confirmation");
                }
                Channel::Pi {
                    endpoint,
                    original: Box::new(original),
                }
            }
            Backend::Claude(endpoint) => {
                if ui_request.action == Action::Pickup {
                    return Err("Pickup adapter is not integrated");
                }
                let subject = claude::Subject::from_session(native)
                    .map_err(|_| "Session changed; review a new confirmation")?;
                let home = std::env::var_os("HOME").ok_or("Home directory unavailable")?;
                Channel::Claude {
                    controller: Box::new(claude::Controller::new(
                        endpoint,
                        claude::Env::new(home.into()),
                    )),
                    subject,
                }
            }
        };
        Ok(Self {
            ui_request,
            channel,
            sent: false,
            progress: None,
        })
    }
    /// The call that sends the effect for this request.
    fn action(&self) -> agentcenter_core::control::pi::Action {
        use agentcenter_core::control::pi;
        match self.ui_request.action {
            Action::Handoff => pi::Action::Handoff,
            // Pickup's start shares the compact slot: any non-status action.
            Action::Compact | Action::Pickup => pi::Action::Compact,
        }
    }
    fn is_claude(&self) -> bool {
        matches!(
            self.channel,
            Channel::Claude { .. } | Channel::Pickup { claude: true, .. }
        )
    }
    /// Call only inside the serialized blocking worker. No retry after write.
    fn exchange(
        &mut self,
        action: agentcenter_core::control::pi::Action,
    ) -> (Status, Option<String>) {
        use agentcenter_core::control::{self as core, pi};
        let start = !matches!(action, pi::Action::Status | pi::Action::Cancel);
        if start && (action != self.action() || self.sent) {
            return (Status::Unknown, None);
        }
        let id = self.ui_request.operation_id.clone();
        match &mut self.channel {
            Channel::Pi { endpoint, original } => {
                let request = match action {
                    pi::Action::Status => original.status(),
                    pi::Action::Cancel => original.cancel(),
                    _ => {
                        self.sent = true;
                        (**original).clone()
                    }
                };
                match pi::exchange(endpoint, &request, std::time::Duration::from_secs(2)) {
                    Ok(reply) => {
                        let status = status_from_core_reply(&self.ui_request, &reply);
                        let completed_at = if matches!(status, Status::Completed(_)) {
                            reply.completed_at
                        } else {
                            None
                        };
                        (status, completed_at)
                    }
                    Err(_) => (Status::Unknown, None),
                }
            }
            Channel::Claude {
                controller,
                subject,
            } => {
                let kind = match self.ui_request.action {
                    Action::Handoff => core::Action::Handoff,
                    _ => core::Action::Compact,
                };
                let outcome = if start {
                    self.sent = true;
                    Ok(controller.start(&id, kind, subject))
                } else {
                    // No cancel in the Claude adapter: a cancel call only reads.
                    controller.status(&id)
                };
                match outcome {
                    Ok(o) => status_from_outcome(&self.ui_request, &o),
                    Err(_) => (Status::Unknown, None),
                }
            }
            Channel::Pickup { port, .. } => {
                // No cancel in the pickup contract: a cancel call only reads.
                let outcome = if start {
                    self.sent = true;
                    port.start(&id)
                } else {
                    port.status(&id)
                };
                match outcome {
                    Ok(progress) if progress.operation_id == id => {
                        let status = status_from_progress(&self.ui_request, &progress);
                        let at = progress.handoff.as_ref().map(|a| a.modified_ms);
                        self.progress = Some(progress);
                        let local = at
                            .filter(|_| matches!(status, Status::Completed(_)))
                            .and_then(|ms| glib::DateTime::from_unix_local((ms / 1000) as i64).ok())
                            .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
                            .map(String::from);
                        (status, local)
                    }
                    _ => (Status::Unknown, None),
                }
            }
        }
    }
}
/// Common contract outcome (CC4) as UI status. Completion needs evidence.
fn status_from_outcome(
    request: &Request,
    o: &agentcenter_core::control::Outcome,
) -> (Status, Option<String>) {
    use agentcenter_core::control::{Evidence as E, State};
    if o.operation_id != request.operation_id {
        return (Status::Unknown, None);
    }
    let local = |ms: u64| {
        glib::DateTime::from_unix_local((ms / 1000) as i64)
            .ok()
            .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").ok())
            .map(String::from)
    };
    match o.state {
        State::Accepted => (Status::Accepted, None),
        State::Running => (Status::Sent, None),
        State::Rejected => (Status::Refused(o.code.into()), None),
        State::Failed => (Status::Failed(o.code.into()), None),
        State::Cancelled => (Status::Cancelled, None),
        State::Unknown => (Status::Unknown, None),
        State::Completed => {
            let (evidence, at) = match &o.evidence {
                Some(E::Persisted(a)) => (
                    Evidence::Persisted {
                        path: a.path.to_string_lossy().into_owned(),
                        size: a.size,
                        sha256: a.sha256.clone(),
                    },
                    local(a.modified_ms),
                ),
                Some(E::Compacted { boundary_ms }) => (
                    Evidence::Compacted {
                        boundary_ms: *boundary_ms,
                    },
                    local(*boundary_ms),
                ),
                None => return (Status::Unknown, None),
            };
            if evidence.valid_for(request) {
                (Status::Completed(evidence), at)
            } else {
                (Status::Unknown, None)
            }
        }
    }
}
// Never render this key: it contains the private instance nonce.
fn endpoint_key(backend: &Backend) -> Option<String> {
    match backend {
        Backend::Pi(endpoint) => {
            let request = endpoint
                .request(
                    "00000000-0000-4000-8000-000000000000".into(),
                    agentcenter_core::control::pi::Action::Status,
                )
                .ok()?;
            Some(serde_json::json!([endpoint.socket, request]).to_string())
        }
        Backend::Claude(e) => Some(
            serde_json::json!(["herdr", e.socket, e.server_pid, e.server_starttime]).to_string(),
        ),
    }
}
/// Production discovery, run only by `gio::spawn_blocking`.
fn authenticated_session(
    native: &agentcenter_core::Session,
) -> Result<(Backend, Session), &'static str> {
    if native.agent == "claude" {
        return claude_session(native);
    }
    let (endpoint, mut session) = pi_session(native)?;
    // Owner (22/10): Pi's pickup exists only inside herdr (CC5 sends the
    // extension command through the pane); outside it, Handoff and Compact.
    let in_herdr =
        agentcenter_core::control::herdr::Target::from_session(native).is_some_and(|t| {
            [&t.workspace_id, &t.tab_id, &t.pane_id]
                .iter()
                .all(|v| !v.is_empty() && !matches!(v.as_str(), "current" | "default"))
        });
    session.pickup_disabled = Some(
        if in_herdr {
            reason::PICKUP_NOT_INTEGRATED
        } else {
            reason::PI_OUTSIDE_HERDR
        }
        .into(),
    );
    Ok((Backend::Pi(Box::new(endpoint)), session))
}
/// Claude Code through the pinned herdr server (CC4 capability).
fn claude_session(native: &agentcenter_core::Session) -> Result<(Backend, Session), &'static str> {
    use agentcenter_core::control::{Capability, claude};
    let subject = claude::Subject::from_session(native)?;
    let endpoint = herdr_server()?;
    let home = std::env::var_os("HOME").ok_or("no_home")?;
    let controller = claude::Controller::new(endpoint.clone(), claude::Env::new(home.into()));
    let caps = controller.capabilities(&subject);
    let why = |c: &Capability| {
        (!c.enabled).then(|| {
            c.disabled_reason
                .clone()
                .unwrap_or_else(|| "unavailable".into())
        })
    };
    let start = native.starttime.ok_or(claude::code::IDENTITY)?;
    let session = Session {
        agent: Agent::Claude,
        identity: serde_json::json!([
            "claude",
            subject.session_id,
            native.pid,
            start,
            subject.target.pane_id
        ])
        .to_string(),
        revision: start.to_string(),
        name: format!("Claude Code (pid {})", native.pid),
        handoff_destination: String::new(),
        expected: serde_json::json!({"sessionId": subject.session_id}),
        handoff_disabled: why(&caps.handoff),
        compact_disabled: why(&caps.compact),
        // CC6 (Claude pickup) is not in this build yet.
        pickup_disabled: Some(reason::PICKUP_NOT_INTEGRATED.into()),
        cancel_supported: caps.handoff.cancel_supported && caps.compact.cancel_supported,
    };
    Ok((Backend::Claude(endpoint), session))
}
/// The one herdr server of this user, by /proc (off the GTK thread only).
/// None or several: no target, never the first.
fn herdr_server() -> Result<agentcenter_core::control::herdr::Endpoint, &'static str> {
    use std::os::unix::fs::MetadataExt;
    let socket = agentcenter_core::overlay::herdr::socket_path(|k| std::env::var_os(k))
        .ok_or("no_herdr_pane")?;
    let uid = std::fs::metadata("/proc/self")
        .map_err(|_| "identity")?
        .uid();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")
        .map_err(|_| "identity")?
        .flatten()
    {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("comm")).map_or(true, |c| c.trim() != "herdr")
            || entry.metadata().map_or(true, |m| m.uid() != uid)
        {
            continue;
        }
        let server = std::fs::read(dir.join("cmdline"))
            .is_ok_and(|c| c.split(|b| *b == 0).any(|a| a == b"server"));
        let start = std::fs::read_to_string(dir.join("stat"))
            .ok()
            .and_then(|t| agentcenter_core::procfs::stat::parse_stat(&t).ok())
            .map(|s| s.starttime);
        if let (true, Some(server_starttime)) = (server, start) {
            found.push((pid, server_starttime));
        }
    }
    match found.as_slice() {
        [(server_pid, server_starttime)] => Ok(agentcenter_core::control::herdr::Endpoint {
            socket,
            server_pid: *server_pid,
            server_starttime: *server_starttime,
        }),
        [] => Err("no_herdr_pane"),
        _ => Err("ambiguous"),
    }
}
fn pi_session(
    native: &agentcenter_core::Session,
) -> Result<(agentcenter_core::control::pi::Endpoint, Session), &'static str> {
    use agentcenter_core::control::pi;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("Private runtime unavailable")?;
    let mut endpoint = discover_endpoint(std::path::Path::new(&runtime), native)?;
    let request = endpoint
        .request(glib::uuid_string_random().to_string(), pi::Action::Status)
        .map_err(|_| "Incompatible endpoint")?;
    let reply = pi::exchange(&endpoint, &request, std::time::Duration::from_secs(2))
        .map_err(|_| "Endpoint authentication failed")?;
    let metadata = reply.metadata.ok_or("Native metadata unavailable")?;
    let expected = metadata.expected.ok_or("Native identity unavailable")?;
    if expected.session_id != endpoint.session_id || expected.session_file != endpoint.session_file
    {
        return Err("Native identity changed");
    }
    endpoint.expected = Some(expected.clone());
    endpoint.handoff_directory = metadata.handoff_directory;
    endpoint.capabilities = metadata.capabilities;
    let directory = endpoint
        .handoff_directory
        .as_ref()
        .and_then(|p| p.to_str())
        .ok_or("Private handoff directory unavailable")?
        .to_owned();
    let value = serde_json::json!({"expected":expected,"capabilities":endpoint.capabilities});
    let session = Session::from_metadata(&value, format!("Pi (pid {})", native.pid), directory)
        .ok_or("Incompatible native metadata")?;
    Ok((endpoint, session))
}
/// Match sampled process and native session identity; ambiguity never picks first.
fn discover_endpoint(
    runtime: &std::path::Path,
    native: &agentcenter_core::Session,
) -> Result<agentcenter_core::control::pi::Endpoint, &'static str> {
    use agentcenter_core::control::pi;
    let id = native
        .overlay
        .as_ref()
        .and_then(|o| o.agent_session_id.as_deref())
        .filter(|id| !id.is_empty())
        .ok_or("Native identity missing")?;
    let start = native
        .starttime
        .ok_or("Process identity missing")?
        .to_string();
    if native.agent != "pi" {
        return Err("Unsupported identity");
    }
    let mut candidates = pi::discover(runtime)
        .map_err(|_| "Private extension unavailable; enable it manually in Pi")?
        .into_iter()
        .filter(|e| e.pid == native.pid && e.process_starttime == start && e.session_id == id);
    let endpoint = candidates
        .next()
        .ok_or("No matching extension; enable it manually in Pi")?;
    if candidates.next().is_some() {
        return Err("Ambiguous native identity; use Pi manually");
    }
    Ok(endpoint)
}
fn status_from_core_reply(
    request: &Request,
    reply: &agentcenter_core::control::pi::Reply,
) -> Status {
    use agentcenter_core::control::pi::State;
    let mut value = serde_json::json!({
        "protocol_version": reply.protocol_version,
        "operation_id": reply.operation_id,
        "code": reply.code,
        "state": match reply.state { State::Accepted => "accepted", State::Running => "running", State::Completed => "completed", State::Rejected => "rejected", State::Failed => "failed", State::Cancelled => "cancelled", State::Unknown => "unknown" },
        "metadata": reply.metadata.as_ref().map(|_| serde_json::json!({})),
        "expected": reply.expected,
        "artifact_path": reply.artifact_path,
    });
    if let Some(c) = &reply.compaction {
        value["compaction"] = serde_json::json!({"capability":c.capability,"sessionId":c.session_id,"sessionFile":c.session_file,"previousLeafId":c.previous_leaf_id,"entryId":c.entry_id});
    }
    status_from_reply(request, &value)
}

fn text(value: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_xalign(0.0);
    label.set_selectable(true);
    label
}
pub fn confirmation_dialog(prompt: &Confirmation) -> adw::AlertDialog {
    let heading = match prompt.action {
        Action::Pickup => "Handoff and start a new session?".to_owned(),
        a => format!("{} session?", a.label()),
    };
    let dialog = adw::AlertDialog::new(Some(&heading), Some(&prompt.body()));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", prompt.action.label());
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog
}

#[cfg(test)]
#[path = "session_control_qa_tests.rs"]
mod qa_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        Session {
            agent: Agent::Pi,
            identity: "native-a".into(),
            revision: "r1".into(),
            name: "Pi".into(),
            handoff_destination: "/private".into(),
            expected: serde_json::json!({"managerId":"m", "revision":"1", "sessionId":"s", "sessionFile":"/session.jsonl", "leafId":"old"}),
            handoff_disabled: None,
            compact_disabled: None,
            pickup_disabled: None,
            cancel_supported: false,
        }
    }
    fn handoff() -> Evidence {
        Evidence::Handoff {
            path: "/private/nonce-op1.md".into(),
            expected: session().expected,
        }
    }
    fn compact() -> Evidence {
        Evidence::Compact {
            capability: "agentcenter.compactGuarded.v1".into(),
            session_id: "s".into(),
            session_file: "/session.jsonl".into(),
            previous_leaf_id: serde_json::json!("old"),
            entry_id: "new".into(),
        }
    }
    #[test]
    fn protocol2_adapter_preserves_original_snapshot_without_inventing_dates() {
        let c = start(Action::Compact);
        let r = c.query().unwrap();
        let mut reply = serde_json::json!({"protocol_version":2,"operation_id":"op1","state":"accepted","code":"accepted"});
        assert_eq!(status_from_reply(r, &reply), Status::Accepted);
        reply["state"] = "completed".into();
        reply["code"] = "compacted".into();
        reply["metadata"] = serde_json::json!({"expected": {"leafId":"new", "revision":"2"}});
        reply["compaction"] = serde_json::json!({"capability":"agentcenter.compactGuarded.v1", "sessionId":"s", "sessionFile":"/session.jsonl", "previousLeafId":"old", "entryId":"new"});
        assert_eq!(status_from_reply(r, &reply), Status::Completed(compact()));
        assert_eq!(
            status_from_reply(r, &reply).text(),
            "Native compact persisted: new"
        );
        assert_eq!(r.session.expected["leafId"], "old");
        reply["compaction"]["previousLeafId"] = "new".into();
        assert_eq!(status_from_reply(r, &reply), Status::Unknown);
    }
    #[test]
    fn handoff_paths_must_be_canonical_direct_children_with_operation_suffix() {
        let c = start(Action::Handoff);
        let r = c.query().unwrap();
        for path in [
            "/private/../private/nonce-op1.md",
            "/private/./nonce-op1.md",
            "/private//nonce-op1.md",
            "/private/sub/nonce-op1.md",
            "/private/nonce-op2.md",
            "private/nonce-op1.md",
            "/private/-op1.md",
        ] {
            assert!(
                !Evidence::Handoff {
                    path: path.into(),
                    expected: session().expected
                }
                .valid_for(r),
                "{path}"
            );
        }
        assert!(handoff().valid_for(r));
        let reply = serde_json::json!({"protocol_version":2,"operation_id":"op1","state":"completed","code":"persisted","metadata":{},"artifact_path":"/private/random-op1.md","expected":session().expected});
        assert!(matches!(
            status_from_reply(r, &reply),
            Status::Completed(Evidence::Handoff { .. })
        ));
    }
    #[test]
    fn metadata_requires_snapshot_and_explicit_capabilities() {
        let metadata = serde_json::json!({"expected":session().expected,"capabilities":{"handoff":{"enabled":true,"disabled_reason":null},"compact":{"enabled":false,"disabled_reason":"busy"}}});
        let s = Session::from_metadata(&metadata, "Pi".into(), "/private".into()).unwrap();
        assert_eq!(s.compact_disabled.as_deref(), Some("busy"));
        assert!(s.handoff_disabled.is_none());
        assert!(!s.cancel_supported);
        let mut cancellable = metadata.clone();
        for action in ["handoff", "compact"] {
            cancellable["capabilities"][action]["cancel_supported"] = true.into();
        }
        assert!(
            Session::from_metadata(&cancellable, "Pi".into(), "/private".into())
                .unwrap()
                .cancel_supported
        );
        assert!(
            Session::from_metadata(&serde_json::json!({}), "Pi".into(), "/private".into())
                .is_none()
        );
        assert!(
            Session::from_metadata(&metadata, "Pi".into(), "/private/../private".into()).is_none()
        );
        let mut c = Controller::default();
        c.select(Some(s));
        assert!(
            c.prepare(Action::Handoff)
                .unwrap()
                .body()
                .contains("private directory /private")
        );
    }
    fn start(action: Action) -> Controller {
        let mut c = Controller::default();
        c.select(Some(session()));
        let p = c.prepare(action).unwrap();
        c.confirm(p, &session(), "op1".into()).unwrap();
        c
    }
    #[test]
    fn confirmation_revalidates_and_double_click_is_blocked() {
        let mut c = Controller::default();
        c.select(Some(session()));
        let p = c.prepare(Action::Handoff).unwrap();
        let mut changed = session();
        changed.revision = "r2".into();
        assert!(c.confirm(p.clone(), &changed, "x".into()).is_err());
        c.select(None);
        c.select(Some(session()));
        assert!(c.confirm(p, &session(), "x".into()).is_err());
        let c = start(Action::Handoff);
        assert!(c.prepare(Action::Compact).is_err());
    }
    #[test]
    fn identical_metadata_preserves_confirmation_but_new_capabilities_invalidate_it() {
        let mut c = Controller::default();
        let s = session();
        c.select(Some(s.clone()));
        let prompt = c.prepare(Action::Handoff).unwrap();
        c.select(Some(s.clone()));
        assert!(c.confirm(prompt, &s, "same".into()).is_ok());
        assert!(c.update("same", "native-a", Status::Failed("quota".into())));
        let prompt = c.prepare(Action::Handoff).unwrap();
        let mut changed = s;
        changed.compact_disabled = Some("busy".into());
        c.select(Some(changed.clone()));
        assert!(c.confirm(prompt, &changed, "changed".into()).is_err());
    }

    #[test]
    fn accepted_running_and_unknown_are_not_completion() {
        let mut c = start(Action::Handoff);
        for status in [Status::Accepted, Status::Running] {
            assert!(c.update("op1", "native-a", status));
            assert!(!c.visible_status().unwrap().terminal());
        }
        c.timeout_after_send();
        assert_eq!(c.visible_status(), Some(&Status::Unknown));
        assert_eq!(c.query().unwrap().operation_id, "op1");
        assert!(c.prepare(Action::Handoff).is_err());
        assert!(!c.update("op1", "native-a", Status::Accepted));
    }
    #[test]
    fn completion_requires_matching_persistence_evidence() {
        for action in [Action::Handoff, Action::Compact] {
            let mut c = start(action);
            assert!(!c.update(
                "op1",
                "native-a",
                Status::Completed(Evidence::Handoff {
                    path: "wrong".into(),
                    expected: session().expected
                })
            ));
            assert_eq!(c.visible_status(), Some(&Status::Unknown));
            let evidence = match action {
                Action::Handoff => handoff(),
                _ => compact(),
            };
            assert!(c.update("op1", "native-a", Status::Completed(evidence)));
            assert!(c.visible_status().unwrap().terminal());
            assert!(!c.update("op1", "native-a", Status::Running));
        }
    }
    #[test]
    fn selection_new_operation_and_late_results_do_not_leak() {
        let mut c = start(Action::Handoff);
        let mut b = session();
        b.identity = "b".into();
        c.select(Some(b.clone()));
        assert!(c.visible_status().is_none());
        assert_eq!(c.query().unwrap().operation_id, "op1");
        assert!(!c.update("wrong", "native-a", Status::Failed("x".into())));
        assert!(c.update("op1", "native-a", Status::Failed("quota".into())));
        assert!(c.visible_status().is_none());
        let p = c.prepare(Action::Compact).unwrap();
        c.confirm(p, &b, "op2".into()).unwrap();
        assert!(!c.update("op1", "native-a", Status::Cancelled));
        assert_eq!(c.visible_status(), Some(&Status::Requested));
    }
    #[test]
    fn cancellation_requires_capability_and_acknowledgement() {
        let mut c = start(Action::Compact);
        assert!(c.request_cancel().is_none());
        let mut s = session();
        s.cancel_supported = true;
        let mut c = Controller::default();
        c.select(Some(s.clone()));
        let p = c.prepare(Action::Compact).unwrap();
        c.confirm(p, &s, "op".into()).unwrap();
        assert!(c.request_cancel().is_some());
        assert!(c.request_cancel().is_none());
        assert_eq!(c.visible_status(), Some(&Status::Requested));
        assert!(c.update("op", "native-a", Status::Cancelled));
        assert!(c.visible_status().unwrap().text().contains("not undone"));
    }
    /// An unknown reply must remain queryable without allowing a second dispatch.
    #[test]
    fn explicit_unknown_keeps_original_request_and_blocks_actions() {
        let mut c = start(Action::Compact);
        let original = c.query().unwrap().clone();
        assert!(c.update("op1", "native-a", Status::Unknown));
        assert_eq!(c.query(), Some(&original));
        for action in [Action::Handoff, Action::Compact] {
            assert!(c.prepare(action).is_err());
        }
        assert!(!c.update("op1", "native-a", Status::Accepted));
        assert_eq!(c.visible_status(), Some(&Status::Unknown));
        assert!(c.update("op1", "native-a", Status::Running));
        assert!(!c.visible_status().unwrap().terminal());
    }

    #[test]
    fn handoff_rejects_missing_expected_and_compact_evidence() {
        for evidence in [
            Evidence::Handoff {
                path: "/private/nonce-op1.md".into(),
                expected: serde_json::Value::Null,
            },
            compact(),
        ] {
            let mut c = start(Action::Handoff);
            assert!(!c.update("op1", "native-a", Status::Completed(evidence)));
            assert_eq!(c.visible_status(), Some(&Status::Unknown));
            assert_eq!(c.query().unwrap().operation_id, "op1");
        }
    }

    #[test]
    fn compact_rejects_wrong_identity_leaf_capability_or_entry() {
        for field in 0..6 {
            let mut evidence = compact();
            if let Evidence::Compact {
                capability,
                session_id,
                session_file,
                previous_leaf_id,
                entry_id,
            } = &mut evidence
            {
                match field {
                    0 => capability.clear(),
                    1 => session_id.push('x'),
                    2 => session_file.push('x'),
                    3 => *previous_leaf_id = serde_json::Value::Null,
                    4 => entry_id.clear(),
                    _ => *entry_id = "old".into(),
                }
            }
            let mut c = start(Action::Compact);
            assert!(!c.update("op1", "native-a", Status::Completed(evidence)));
            assert_eq!(c.visible_status(), Some(&Status::Unknown));
            assert!(c.query().is_some());
        }
    }

    #[test]
    fn handoff_requires_private_directory_and_reply_identity() {
        let mut c = start(Action::Handoff);
        let valid = handoff();
        assert!(!c.update("op1", "another-session", Status::Completed(valid.clone())));
        assert_eq!(c.visible_status(), Some(&Status::Requested));
        assert!(!c.update(
            "op1",
            "native-a",
            Status::Completed(Evidence::Handoff {
                path: "/private/other/nonce-op1.md".into(),
                expected: session().expected,
            })
        ));
        assert_eq!(c.visible_status(), Some(&Status::Unknown));
        assert!(c.update("op1", "native-a", Status::Completed(valid)));
    }

    /// Identity/revision changes and selection ABA must invalidate a stale dialog.
    #[test]
    fn stale_confirmation_cannot_dispatch_or_consume_operation_id() {
        for change in 0..3 {
            let mut c = Controller::default();
            let s = session();
            c.select(Some(s.clone()));
            let prompt = c.prepare(Action::Compact).unwrap();
            let mut current = s.clone();
            match change {
                0 => current.identity = "native-b".into(),
                1 => current.revision = "r2".into(),
                _ => {
                    c.select(None);
                    c.select(Some(s.clone()));
                }
            }
            assert!(c.confirm(prompt, &current, "fresh".into()).is_err());
            assert!(c.query().is_none());
            let prompt = c.prepare(Action::Compact).unwrap();
            assert!(c.confirm(prompt, &s, "fresh".into()).is_ok());
        }
    }

    /// A cancel request is sent once; a late completion can still win before ack.
    #[test]
    fn repeated_cancel_does_not_mask_persistence_after_selection_change() {
        let mut s = session();
        s.cancel_supported = true;
        let mut c = Controller::default();
        c.select(Some(s.clone()));
        let prompt = c.prepare(Action::Compact).unwrap();
        c.confirm(prompt, &s, "cancel-op".into()).unwrap();
        assert_eq!(c.request_cancel().unwrap().operation_id, "cancel-op");
        assert!(c.request_cancel().is_none());
        c.select(None);
        assert!(c.visible_status().is_none());
        let done = Status::Completed(compact());
        assert!(c.update("cancel-op", "native-a", done.clone()));
        assert!(c.visible_status().is_none());
        assert!(c.request_cancel().is_none());
        assert!(!c.update("cancel-op", "native-a", Status::Cancelled));
        c.select(Some(s));
        assert_eq!(c.visible_status(), Some(&done));
    }

    /// Empty and previously terminal operation IDs cannot become another request.
    #[test]
    fn operation_ids_are_not_reusable_after_terminal_failure() {
        let mut c = start(Action::Compact);
        assert!(c.update("op1", "native-a", Status::Failed("quota".into())));
        for id in ["", "op1"] {
            let prompt = c.prepare(Action::Compact).unwrap();
            assert!(c.confirm(prompt, &session(), id.into()).is_err());
            assert_eq!(c.visible_status(), Some(&Status::Failed("quota".into())));
            assert!(c.query().is_none());
        }
        let prompt = c.prepare(Action::Compact).unwrap();
        assert!(c.confirm(prompt, &session(), "op2".into()).is_ok());
        assert!(!c.update("op1", "native-a", Status::Cancelled));
        assert_eq!(c.query().unwrap().operation_id, "op2");
        assert_eq!(c.visible_status(), Some(&Status::Requested));
    }

    #[test]
    fn reasons_and_terminal_failures_are_explicit() {
        for why in [
            "Extension missing; enable it manually",
            "Busy",
            "Pending",
            "Ambiguous identity",
            "Incompatible version",
            "Model unavailable",
            "Unsupported agent",
            "Session exited",
        ] {
            let mut s = session();
            s.handoff_disabled = Some(why.into());
            s.compact_disabled = Some(why.into());
            let mut c = Controller::default();
            c.select(Some(s));
            for a in [Action::Handoff, Action::Compact] {
                assert_eq!(c.prepare(a).unwrap_err(), why);
            }
        }
        for status in [
            Status::Refused("quota".into()),
            Status::Failed("exited".into()),
            Status::Cancelled,
        ] {
            let mut c = start(Action::Compact);
            assert!(c.update("op1", "native-a", status.clone()));
            assert_eq!(c.visible_status(), Some(&status));
            assert!(c.query().is_none());
        }
    }
}
