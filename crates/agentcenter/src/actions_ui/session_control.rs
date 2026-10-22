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
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Handoff => "Handoff",
            Self::Compact => "Compact",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
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
            identity: serde_json::json!([manager, id, file]).to_string(),
            revision: revision.to_owned(),
            name,
            handoff_destination: directory,
            expected: expected.clone(),
            handoff_disabled: reason("handoff")?,
            compact_disabled: reason("compact")?,
            cancel_supported: ["handoff", "compact"].iter().all(|action| {
                metadata["capabilities"][action]["cancel_supported"].as_bool() == Some(true)
            }),
        })
    }
    pub fn disabled_reason(&self, action: Action) -> Option<&str> {
        if self.identity.is_empty() || self.revision.is_empty() {
            return Some("Session identity or revision is unavailable");
        }
        if action == Action::Handoff && self.handoff_destination.is_empty() {
            return Some("Private handoff destination is unavailable");
        }
        match action {
            Action::Handoff => self.handoff_disabled.as_deref(),
            Action::Compact => self.compact_disabled.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Confirmation {
    session: Session,
    action: Action,
    generation: u64,
}
impl Confirmation {
    pub fn body(&self) -> String {
        let effect = match self.action {
            Action::Handoff => format!(
                "Sends projected session text to the provider currently selected in Pi to generate a handoff. Saves it in the private directory {}. The producer chooses the filename; the GUI does not read its content.",
                self.session.handoff_destination
            ),
            Action::Compact => {
                "Compacts native context. The raw native history remains available.".into()
            }
        };
        format!(
            "Session: {}\nIdentity: {}\nRevision: {}\n\n{effect}\nThis may consume tokens with the current model. Does not clear, create, close, shut down or resume a session.",
            self.session.name, self.session.identity, self.session.revision
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
                (Status::Running | Status::Unknown, Status::Accepted)
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
type Authenticate = fn(
    &agentcenter_core::Session,
) -> Result<(agentcenter_core::control::pi::Endpoint, Session), &'static str>;

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
    refresh_button: gtk::Button,
    completed_at: RefCell<Option<String>>,
    refresh_pending: Cell<bool>,
    cancel_pending: Cell<bool>,
    controller: RefCell<Controller>,
    native: RefCell<Option<agentcenter_core::Session>>,
    operation: RefCell<Option<TransportOperation>>,
    busy: Cell<bool>,
    epoch: Cell<u64>,
    endpoint_key: RefCell<Option<String>>,
    authenticate: Cell<Authenticate>,
}
impl SessionControls {
    pub fn new(live: bool) -> Self {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let buttons = [Action::Handoff, Action::Compact]
            .into_iter()
            .map(|a| {
                let b = gtk::Button::with_label(&format!("{}…", a.label()));
                b.set_height_request(44);
                widget.append(&b);
                (a, b)
            })
            .collect();
        let reason = text("Select a session");
        widget.append(&reason);
        let query = gtk::Button::with_label("Query operation");
        let cancel = gtk::Button::with_label("Request cancellation");
        let refresh_button = gtk::Button::with_label("Refresh capabilities");
        for b in [&query, &cancel, &refresh_button] {
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
            refresh_button,
            completed_at: RefCell::new(None),
            refresh_pending: Cell::new(false),
            cancel_pending: Cell::new(false),
            controller: RefCell::new(Controller::default()),
            native: RefCell::new(None),
            operation: RefCell::new(None),
            busy: Cell::new(false),
            epoch: Cell::new(0),
            endpoint_key: RefCell::new(None),
            authenticate: Cell::new(authenticated_session),
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
                    } else {
                        p.reconcile(action);
                    }
                }
            });
        }
        let weak = Rc::downgrade(&panel);
        panel.refresh_button.connect_clicked(move |_| {
            if let Some(p) = weak.upgrade() {
                p.refresh();
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
        if p.native.borrow().as_ref().map(key) == session.map(key) {
            return;
        }
        *p.native.borrow_mut() = session.cloned();
        p.epoch.set(p.epoch.get().wrapping_add(1));
        p.controller.borrow_mut().select(None);
        *p.endpoint_key.borrow_mut() = None;
        let why = selection_blocker(p.live, session);
        p.refresh_pending.set(why.is_empty());
        p.render();
        p.reason.set_text(why);
        if why.is_empty() {
            p.refresh();
        }
    }
}
impl Panel {
    fn render(&self) {
        let c = self.controller.borrow();
        // Owner rule (22/10): a control the session cannot run is not shown.
        // An unresolved operation stays reachable after the selection moves.
        let supported = c.selection.is_some()
            || selection_blocker(self.live, self.native.borrow().as_ref()).is_empty();
        for (a, b) in &self.buttons {
            b.set_visible(supported);
            b.set_sensitive(!self.busy.get() && c.disabled_reason(*a).is_none());
        }
        self.refresh_button.set_visible(supported);
        self.query.set_visible(c.query().is_some());
        self.cancel
            .set_visible(c.query().is_some_and(|r| r.session.cancel_supported));
        self.query
            .set_sensitive(!self.busy.get() && c.query().is_some());
        // A supported cancel may queue behind the one bounded read in flight.
        // Polling must not make cancellation permanently inaccessible.
        self.cancel.set_sensitive(
            !c.cancel_requested && c.query().is_some_and(|r| r.session.cancel_supported),
        );
        if let Some(request) = c.query() {
            self.query
                .set_label(&format!("Query operation for {}", request.session.name));
            self.cancel.set_label(&format!(
                "Request cancellation for {}",
                request.session.name
            ));
        }
        self.refresh_button.set_sensitive(
            !self.busy.get()
                && selection_blocker(self.live, self.native.borrow().as_ref()).is_empty(),
        );
        let availability = c.selection.as_ref().map(|_| {
            let mut message = format!(
                "Handoff: {}\nCompact: {}",
                c.disabled_reason(Action::Handoff).unwrap_or("Available"),
                c.disabled_reason(Action::Compact).unwrap_or("Available")
            );
            if let Some(request) = c.query() {
                message.push_str(&format!(
                    "\nUnresolved operation for {}. Query or cancel that operation before starting another.",
                    request.session.name
                ));
            }
            message
        });
        if let Some(s) = c.visible_status() {
            let mut message = s.text();
            if matches!(s, Status::Completed(_))
                && let Some(date) = self.completed_at.borrow().as_ref()
            {
                message.push_str(&format!("\nPersisted at: {date}"));
            }
            if c.cancel_requested && c.query().is_some() {
                message.push_str("\nCancellation requested; waiting for acknowledgement.");
            }
            if let Some(availability) = availability {
                message.push_str(&format!("\n{availability}"));
            }
            self.reason.set_text(&message);
        } else if let Some(availability) = availability {
            self.reason.set_text(&availability);
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
        self.render();
        let epoch = self.epoch.get();
        let authenticate = self.authenticate.get();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || authenticate(&native)).await;
            let Some(p) = weak.upgrade() else {
                return;
            };
            p.busy.set(false);
            if epoch != p.epoch.get() {
                p.refresh();
                return;
            }
            match result {
                Ok(Ok((endpoint, s))) => {
                    *p.endpoint_key.borrow_mut() = endpoint_key(&endpoint);
                    p.controller.borrow_mut().select(Some(s));
                    p.render();
                }
                _ => {
                    p.controller.borrow_mut().select(None);
                    p.render();
                    p.reason.set_text("Private extension unavailable, incompatible or ambiguous. Enable the AgentCenter extension manually in Pi, then use Refresh capabilities.");
                }
            }
        });
    }
    fn prepare(self: &Rc<Self>, action: Action) {
        if self.busy.get() {
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
        d.present(Some(&self.widget));
    }
    fn dispatch(self: &Rc<Self>, prompt: Confirmation) {
        if self.busy.replace(true) {
            return;
        }
        let Some(native) = self.native.borrow().clone() else {
            self.busy.set(false);
            return;
        };
        let epoch = self.epoch.get();
        let authenticate = self.authenticate.get();
        let weak = Rc::downgrade(self);
        self.render();
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
                p.render();
                p.reason.set_text(
                    "Identity could not be revalidated; nothing dispatched. Use Refresh capabilities.",
                );
                return;
            };
            if endpoint_key(&endpoint).as_ref() != p.endpoint_key.borrow().as_ref() {
                *p.endpoint_key.borrow_mut() = endpoint_key(&endpoint);
                p.controller.borrow_mut().select(Some(current));
                p.render();
                p.reason
                    .set_text("Endpoint or metadata changed; review a new confirmation.");
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
                    match TransportOperation::new(endpoint, r) {
                        Ok(op) => {
                            *p.operation.borrow_mut() = Some(op);
                            p.reconcile_dispatch();
                        }
                        Err(why) => {
                            p.controller.borrow_mut().timeout_after_send();
                            p.reason.set_text(why);
                            p.render();
                        }
                    }
                }
                Err(why) => {
                    // Updated metadata requires a fresh confirmation, not reselecting.
                    p.controller.borrow_mut().select(Some(current));
                    p.render();
                    p.reason.set_text(why);
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
        let action = self.operation.borrow().as_ref().map(|o| o.original.action);
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
                Ok((op, status, date)) => {
                    let poll = matches!(status, Status::Accepted | Status::Running);
                    let completed = matches!(status, Status::Completed(_));
                    if status.terminal() {
                        p.cancel_pending.set(false);
                    }
                    let updated = p.controller.borrow_mut().update(
                        &op.ui_request.operation_id,
                        &op.ui_request.session.identity,
                        status,
                    );
                    if updated && completed {
                        p.completed_at.replace(date);
                    }
                    *p.operation.borrow_mut() = Some(op);
                    p.busy.set(poll);
                    p.render();
                    if poll {
                        let weak = Rc::downgrade(&p);
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(300),
                            move || {
                                if let Some(p) = weak.upgrade() {
                                    p.busy.set(false);
                                    p.reconcile(if p.cancel_pending.get() {
                                        pi::Action::Cancel
                                    } else {
                                        pi::Action::Status
                                    });
                                }
                            },
                        );
                    } else if p.cancel_pending.get() {
                        p.reconcile(pi::Action::Cancel);
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
fn selection_blocker(live: bool, session: Option<&agentcenter_core::Session>) -> &'static str {
    if !live {
        return "Actions are off: recorded snapshot. Use the native agent manually.";
    }
    let Some(session) = session else {
        return "Select a session";
    };
    if session.agent != "pi" {
        return "Handoff and Compact are not available for this agent yet.";
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

/// Keep the authenticated endpoint and ORIGINAL request across every read.
struct TransportOperation {
    endpoint: agentcenter_core::control::pi::Endpoint,
    original: agentcenter_core::control::pi::Request,
    ui_request: Request,
    sent: bool,
}
impl TransportOperation {
    fn new(
        endpoint: agentcenter_core::control::pi::Endpoint,
        ui_request: Request,
    ) -> Result<Self, &'static str> {
        use agentcenter_core::control::pi;
        let action = match ui_request.action {
            Action::Handoff => pi::Action::Handoff,
            Action::Compact => pi::Action::Compact,
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
        Ok(Self {
            endpoint,
            original,
            ui_request,
            sent: false,
        })
    }
    /// Call only inside the serialized blocking worker. No retry after write.
    fn exchange(
        &mut self,
        action: agentcenter_core::control::pi::Action,
    ) -> (Status, Option<String>) {
        use agentcenter_core::control::pi;
        let request = match action {
            pi::Action::Status => self.original.status(),
            pi::Action::Cancel => self.original.cancel(),
            _ if action == self.original.action && !self.sent => {
                self.sent = true;
                self.original.clone()
            }
            _ => return (Status::Unknown, None),
        };
        match pi::exchange(&self.endpoint, &request, std::time::Duration::from_secs(2)) {
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
}
// Never render this key: it contains the private instance nonce.
fn endpoint_key(endpoint: &agentcenter_core::control::pi::Endpoint) -> Option<String> {
    let request = endpoint
        .request(
            "00000000-0000-4000-8000-000000000000".into(),
            agentcenter_core::control::pi::Action::Status,
        )
        .ok()?;
    Some(serde_json::json!([endpoint.socket, request]).to_string())
}
fn authenticated_session(
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
    let dialog = adw::AlertDialog::new(
        Some(&format!("{} session?", prompt.action.label())),
        Some(&prompt.body()),
    );
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
            identity: "native-a".into(),
            revision: "r1".into(),
            name: "Pi".into(),
            handoff_destination: "/private".into(),
            expected: serde_json::json!({"managerId":"m", "revision":"1", "sessionId":"s", "sessionFile":"/session.jsonl", "leafId":"old"}),
            handoff_disabled: None,
            compact_disabled: None,
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
                Action::Compact => compact(),
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
